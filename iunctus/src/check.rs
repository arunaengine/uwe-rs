//! `iunctus check <workflow>`: everything that would stop a real run, found at once and without
//! running a task, in every language: unsupported features, syntax and load errors, broken
//! references like missing includes, inputs that do not fit the schema of the workflow, local
//! files that tasks read and that do not exist, and tasks without a container image.
//!
//! The check plans the run with the simulator, in a scratch folder that it removes, so that it
//! goes past the missing files that would stop a real run at the first one. It records nothing
//! in the run database.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use input_schema::Severity;
use workflow_graph::{
    Driver, DriverError, Engine, ExecuteError, ExecutionGraph, Inputs, Language, Simulator,
};

use crate::arguments::Arguments;
use crate::failure::{FAILED, UNSUPPORTED, without_error_numbers};
use crate::setup::Setup;
use crate::{Failure, cache_directory};

/// How many missing files of one step the report names; it counts the others.
const NAMED_FILES: usize = 3;

/// How bad a problem is, the worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A feature that iunctus does not support: a run stops with exit code 33.
    Unsupported,
    /// A run stops.
    Error,
    /// A run goes on, and the user should know.
    Warning,
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Kind::Unsupported => "unsupported",
            Kind::Error => "error",
            Kind::Warning => "warning",
        })
    }
}

/// A problem of the workflow, in the form of every error: where, what, and what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Problem {
    kind: Kind,
    message: String,
}

/// What the check found.
#[derive(Default)]
struct Report {
    problems: Vec<Problem>,
    /// What the check could not look at, like files at remote addresses.
    notes: Vec<String>,
}

impl Report {
    fn add(&mut self, kind: Kind, message: impl Into<String>) {
        self.problems.push(Problem {
            kind,
            message: without_error_numbers(&message.into()),
        });
    }

    /// The error of a driver: one problem per line, with the lines that continue it.
    fn driver_error(&mut self, error: &DriverError) {
        let kind = match error.unsupported {
            true => Kind::Unsupported,
            false => Kind::Error,
        };
        self.add_lines(kind, &error.message);
    }

    /// The error that ended the planned run.
    fn execute_error(&mut self, error: &ExecuteError) {
        match error {
            ExecuteError::Driver(error) => self.driver_error(error),
            error if error.unsupported() => self.add_lines(Kind::Unsupported, &error.to_string()),
            error => self.add_lines(Kind::Error, &error.to_string()),
        }
    }

    fn add_lines(&mut self, kind: Kind, text: &str) {
        for line in text.lines() {
            match self.problems.last_mut() {
                Some(last) if line.starts_with(char::is_whitespace) => {
                    last.message.push('\n');
                    last.message.push_str(line);
                }
                _ if line.trim().is_empty() => {}
                _ => self.add(kind, line.trim_start_matches("iunctus: ")),
            }
        }
    }

    /// The values of the run against the schema of the inputs, as a real run checks them, and
    /// where the schema and the inputs that the code reads differ.
    fn inputs(&mut self, inputs: &Inputs) {
        for problem in input_schema::check(inputs, false) {
            let kind = match problem.severity {
                Severity::Error => Kind::Error,
                Severity::Warning => Kind::Warning,
            };
            self.add(kind, problem.message);
        }
        if let Some(read) = &inputs.read {
            for warning in input_schema::compare(&inputs.schema, read) {
                self.add(Kind::Warning, warning);
            }
        }
    }

    /// The tasks of the planned run: those without a container image, when `-default-image`
    /// gives none, and the local files they read that do not exist. Files in `scratch` are
    /// iunctus's own, like the made-up values of inputs that have none.
    fn tasks(&mut self, graph: &ExecutionGraph, default_image: Option<&str>, scratch: &Path) {
        let mut without_image: BTreeMap<&str, usize> = BTreeMap::new();
        let mut missing: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
        let mut remote = 0;
        for task in graph.nodes.iter().map(|node| &node.task) {
            if task.image.is_none() && !task.is_no_op() && default_image.is_none() {
                *without_image.entry(&task.step).or_default() += 1;
            }
            for input in &task.inputs {
                if input.content.is_some() || input.source.is_empty() {
                    continue;
                }
                if input.source.contains("://") {
                    remote += 1;
                    continue;
                }
                let source = PathBuf::from(&input.source);
                if source.starts_with(scratch) {
                    continue;
                }
                let files = missing.entry(&task.step).or_default();
                if !source.exists() && !files.contains(&source) {
                    files.push(source);
                }
            }
        }
        for (step, files) in missing.into_iter().filter(|(_, files)| !files.is_empty()) {
            let problem = missing_files(step, &files);
            // The check of the inputs may have named the file already.
            if !self.names_any(&files) {
                self.add(Kind::Error, problem);
            }
        }
        for (step, count) in without_image {
            let tasks = match count {
                1 => "1 task names".to_owned(),
                count => format!("{count} tasks name"),
            };
            self.add(
                Kind::Error,
                format!(
                    "step {step}: {tasks} no container image; iunctus runs every task in a \
                     container, so name an image in the workflow or give one with \
                     `-default-image <image>`"
                ),
            );
        }
        if remote > 0 {
            let inputs = match remote {
                1 => "1 input of a task is".to_owned(),
                count => format!("{count} inputs of tasks are"),
            };
            self.notes.push(format!(
                "{inputs} at remote addresses, which the check does not fetch"
            ));
        }
        let marked = graph.marked().count();
        if marked > 0 {
            let points = match marked {
                1 => "1 point".to_owned(),
                count => format!("{count} points"),
            };
            self.notes.push(format!(
                "the plan has {points} where real data can add or remove tasks; the check cannot \
                 see the problems of tasks that only real data makes"
            ));
        }
    }

    /// Whether a problem so far names one of the files.
    fn names_any(&self, files: &[PathBuf]) -> bool {
        files.iter().any(|file| {
            let names = [shown(file), file.display().to_string()];
            (self.problems.iter()).any(|problem| {
                names
                    .iter()
                    .any(|name| problem.message.contains(name.as_str()))
            })
        })
    }

    /// The exit code of the check: 33 when a feature is not supported, else 1 when a run
    /// would stop, else 0.
    fn code(&self) -> u8 {
        match self.problems.iter().map(|problem| problem.kind).min() {
            Some(Kind::Unsupported) => UNSUPPORTED,
            Some(Kind::Error) => FAILED,
            _ => 0,
        }
    }

    /// The report: the problems, the worst first, what the check could not see, and one line
    /// that sums it up.
    fn text(&self, workflow: &str, graph: Option<&ExecutionGraph>) -> String {
        let mut problems = self.problems.clone();
        problems.sort_by_key(|problem| problem.kind);
        let mut text = String::new();
        for problem in &problems {
            text.push_str(&format!("{}: {}\n", problem.kind, problem.message));
        }
        for note in &self.notes {
            text.push_str(&format!("note: {note}\n"));
        }
        let count = |kind: Kind, one: &str, many: &str| match problems
            .iter()
            .filter(|problem| problem.kind == kind)
            .count()
        {
            0 => None,
            1 => Some(format!("1 {one}")),
            number => Some(format!("{number} {many}")),
        };
        let stopping: Vec<String> = [
            count(
                Kind::Unsupported,
                "unsupported feature",
                "unsupported features",
            ),
            count(Kind::Error, "error", "errors"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let warnings = count(Kind::Warning, "warning", "warnings");
        let summary = match stopping.as_slice() {
            [] => {
                let planned = graph.map(planned).unwrap_or_default();
                format!("nothing would stop a real run{planned}")
            }
            [one] => format!("{one} would stop a real run"),
            [first, second] => format!("{first} and {second} would stop a real run"),
            _ => unreachable!("two kinds of problems stop a run"),
        };
        match warnings {
            Some(warnings) => text.push_str(&format!("{workflow}: {summary}; {warnings}.\n")),
            None => text.push_str(&format!("{workflow}: {summary}.\n")),
        }
        text
    }
}

/// ` of 3 tasks in 2 steps`, what the plan holds.
fn planned(graph: &ExecutionGraph) -> String {
    let tasks = graph.nodes.len();
    let steps = graph.step_graph().steps.len();
    let plural = |number: usize, word: &str| match number {
        1 => format!("1 {word}"),
        _ => format!("{number} {word}s"),
    };
    format!(" of {} in {}", plural(tasks, "task"), plural(steps, "step"))
}

/// The problem of the files of a step that do not exist: each one, or the first few and how
/// many more.
fn missing_files(step: &str, files: &[PathBuf]) -> String {
    let names: Vec<String> = files.iter().map(|file| shown(file)).collect();
    match names.as_slice() {
        [one] => format!("step {step} reads {one}, which does not exist"),
        [others @ .., last] if names.len() <= NAMED_FILES => format!(
            "step {step} reads {} and {last}, which do not exist",
            others.join(", ")
        ),
        names => format!(
            "step {step} reads {} files that do not exist, like {} and {}",
            names.len(),
            names[0],
            names[1]
        ),
    }
}

/// A path as the user named it: relative to the current directory when it is inside it.
fn shown(path: &Path) -> String {
    let current = std::env::current_dir().unwrap_or_default();
    match path.strip_prefix(&current) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        _ => path.display().to_string(),
    }
}

/// The name of a language as its users write it.
fn language_name(language: Language) -> &'static str {
    match language {
        Language::Nextflow => "Nextflow",
        Language::Snakemake => "Snakemake",
        Language::Cwl => "CWL",
        Language::Argo => "Argo",
    }
}

/// `iunctus check`: prints what would stop a real run of the workflow with the same options,
/// and exits with 33 when a feature is not supported, else 1 when something would stop the
/// run, else 0.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let (target, language) = crate::target_and_language(arguments)?;
    let setup = Setup::load(arguments)?;
    let mut launch = crate::launch(arguments, target.clone());
    // The plan goes past the files that are missing, which the check reports.
    launch.simulated = true;
    let run = crate::run_id();
    let mut report = Report::default();
    let graph = match crate::driver(language, &launch, &run, true, arguments, &setup) {
        Ok(mut driver) => plan(driver.as_mut(), &target, &run, arguments, &mut report),
        Err(error) => {
            report.driver_error(&error);
            None
        }
    };
    let workflow = format!("{} ({})", shown(&target), language_name(language));
    print!("{}", report.text(&workflow, graph.as_ref()));
    match report.code() {
        0 => Ok(()),
        code => Err(Failure::said(code)),
    }
}

/// Checks the inputs, then plans the run with the simulator and checks its tasks. The graph
/// of the plan, unless the workflow did not load.
fn plan(
    driver: &mut dyn Driver,
    target: &Path,
    run: &str,
    arguments: &Arguments,
    report: &mut Report,
) -> Option<ExecutionGraph> {
    match driver.inputs() {
        Ok(Some(inputs)) => report.inputs(&inputs),
        Ok(None) => {}
        Err(error) => {
            report.driver_error(&error);
            return None;
        }
    }
    let scratch = cache_directory().join("scratch");
    let simulated = scratch.join("check").join(run);
    let mut simulator = Simulator::new(simulated.clone());
    simulator.verbose = false;
    let workflow = target.to_string_lossy();
    let mut execution = Engine::new(&mut simulator).run(&workflow, driver);
    execution.graph.simulate();
    // The simulator made up its files in the scratch folder, which the check leaves out.
    report.tasks(
        &execution.graph,
        arguments.default_image.as_deref(),
        &scratch,
    );
    if let Some(error) = &execution.error {
        report.execute_error(error);
    }
    let _ = std::fs::remove_dir_all(&simulated);
    Some(execution.graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_problem_decides_the_exit_code() {
        let mut report = Report::default();
        assert_eq!(report.code(), 0);
        report.add(Kind::Warning, "w");
        assert_eq!(report.code(), 0);
        report.add(Kind::Error, "e");
        assert_eq!(report.code(), FAILED);
        report.add(Kind::Unsupported, "u");
        assert_eq!(report.code(), UNSUPPORTED);
        let text = report.text("main.nf (Nextflow)", None);
        assert_eq!(
            text,
            "unsupported: u\nerror: e\nwarning: w\nmain.nf (Nextflow): 1 unsupported feature \
             and 1 error would stop a real run; 1 warning.\n"
        );
    }

    #[test]
    fn lines_that_continue_a_message_stay_with_it() {
        let mut report = Report::default();
        let error = DriverError::new(
            "iunctus: WorkflowError: Missing input files for rule copy:\n    affected files:\n",
        );
        report.driver_error(&error);
        assert_eq!(
            report.problems,
            [Problem {
                kind: Kind::Error,
                message: "WorkflowError: Missing input files for rule copy:\n    affected files:"
                    .into()
            }]
        );
    }

    #[test]
    fn many_missing_files_are_counted() {
        let files: Vec<PathBuf> = ["/d/a", "/d/b", "/d/c", "/d/e"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            missing_files("A", &files[..1]),
            "step A reads /d/a, which does not exist"
        );
        assert_eq!(
            missing_files("A", &files[..2]),
            "step A reads /d/a and /d/b, which do not exist"
        );
        assert_eq!(
            missing_files("A", &files),
            "step A reads 4 files that do not exist, like /d/a and /d/b"
        );
    }
}
