//! Finds workflow planning problems without running tasks or recording a run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use input_schema::Severity;
use workflow_graph::{
    Driver, DriverError, Engine, ExecuteError, ExecutionGraph, Inputs, Language, Simulator,
};

use crate::arguments::Arguments;
use crate::failure::{FAILED, UNSUPPORTED, continues, without_error_numbers};
use crate::setup::Setup;
use crate::{Failure, cache_directory};

/// How many missing files of one step the report names; it counts the others.
const NAMED_FILES: usize = 3;

/// How bad a problem is, the worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A feature that uwe does not support: a run stops with exit code 33.
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
                Some(last) if !line.trim().is_empty() && continues(line) => {
                    last.message.push('\n');
                    last.message.push_str(line);
                }
                _ if line.trim().is_empty() => {}
                _ => self.add(kind, line.trim_start_matches("uwe: ")),
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

    /// The CWL steps that run an abstract `Operation`, the tasks that do nothing, which a real
    /// run refuses as cwltool does.
    fn operations(&mut self, graph: &ExecutionGraph) {
        let tasks = graph.nodes.iter().map(|node| &node.task);
        let steps: BTreeSet<&str> = (tasks.filter(|task| task.is_no_op()))
            .map(|task| task.step.as_str())
            .collect();
        for step in steps {
            self.add(
                Kind::Error,
                format!("step {step} runs an abstract Operation: Workflow has unrunnable abstract Operation"),
            );
        }
    }

    /// The tasks of the planned run: those without a container image, when `-default-image`
    /// gives none, and the local files they read that do not exist. Files in `scratch` are
    /// uwe's own, like the made-up values of inputs that have none.
    fn tasks(
        &mut self,
        graph: &ExecutionGraph,
        default_image: Option<&str>,
        scratch: &Path,
        conda: bool,
    ) {
        let mut without_image: BTreeMap<&str, usize> = BTreeMap::new();
        // Each missing file, with the steps that read it.
        let mut missing: BTreeMap<PathBuf, BTreeSet<&str>> = BTreeMap::new();
        let mut remote = 0;
        let mut missing_conda = BTreeSet::new();
        for task in graph.nodes.iter().map(|node| &node.task) {
            if !conda
                && task
                    .image
                    .as_deref()
                    .or(default_image)
                    .is_some_and(|image| image.starts_with("conda-env:"))
            {
                missing_conda.insert(task.step.as_str());
            }
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
                if !source.starts_with(scratch) && !source.exists() {
                    missing.entry(source).or_default().insert(&task.step);
                }
            }
        }
        // The check of the inputs may have named a file already. The others go together when
        // the same steps read them.
        let mut groups: BTreeMap<BTreeSet<&str>, Vec<PathBuf>> = BTreeMap::new();
        for (file, steps) in missing {
            if !self.names(&file) {
                groups.entry(steps).or_default().push(file);
            }
        }
        for (steps, files) in groups {
            let steps: Vec<&str> = steps.into_iter().collect();
            self.add(Kind::Error, missing_files(&steps, &files));
        }
        for (step, count) in without_image {
            let tasks = match count {
                1 => "1 task names".to_owned(),
                count => format!("{count} tasks name"),
            };
            self.add(
                Kind::Error,
                format!(
                    "step {step}: {tasks} no container image; uwe runs every task in a \
                     container, so name an image in the workflow or give one with \
                     `-default-image <image>`"
                ),
            );
        }
        for step in missing_conda {
            self.add(
                Kind::Unsupported,
                format!("step {step}: Conda preparation requires explicit [conda] configuration with a pinned base, target capabilities, repodata and packages"),
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

    /// Whether a problem so far names the file.
    fn names(&self, file: &Path) -> bool {
        let names = [shown(file), file.display().to_string()];
        (self.problems.iter())
            .any(|problem| (names.iter()).any(|name| problem.message.contains(name.as_str())))
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

/// The problem of files that do not exist and that the same steps read: each file, or the
/// first few and how many more.
fn missing_files(steps: &[&str], files: &[PathBuf]) -> String {
    let (steps, read) = match steps {
        [one] => (format!("step {one}"), "reads"),
        steps => (format!("steps {}", listed(steps)), "read"),
    };
    let names: Vec<String> = files.iter().map(|file| shown(file)).collect();
    match names.len() {
        1 => format!("{steps} {read} {}, which does not exist", names[0]),
        count if count <= NAMED_FILES => {
            format!("{steps} {read} {}, which do not exist", listed(&names))
        }
        count => format!(
            "{steps} {read} {count} files that do not exist, like {} and {}",
            names[0], names[1]
        ),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn listed(names: &[impl AsRef<str>]) -> String {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, others)) => format!("{} and {last}", others.join(", ")),
        None => String::new(),
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
        Language::Wir => "wir",
    }
}

/// `uwe check`: prints what would stop a real run of the workflow with the same options,
/// and exits with 33 when a feature is not supported, else 1 when something would stop the
/// run, else 0.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let setup = Setup::load(arguments)?;
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let (target, language) = crate::target_and_language(arguments)?;
    let mut launch = crate::launch(arguments, target.clone());
    // The plan goes past the files that are missing, which the check reports.
    launch.simulated = true;
    let run = crate::run_id();
    let mut report = Report::default();
    let cancel = crate::stop_on_interrupt();
    let _stop = cancel.clone().drop_guard();
    let graph = match crate::driver(language, &launch, &run, true, arguments, &setup, &cancel) {
        Ok(mut driver) => plan(
            driver.as_mut(),
            &target,
            &run,
            arguments,
            &mut report,
            &cancel,
            setup.conda.is_some(),
        ),
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

/// Whether the simulator finished a task, so that later values come from made-up outputs.
fn made_outputs(graph: &ExecutionGraph) -> bool {
    graph.nodes.iter().any(|node| node.exit_code == Some(0))
}

/// Checks the inputs, then plans the run with the simulator and checks its tasks. The graph
/// of the plan, unless the workflow did not load.
fn plan(
    driver: &mut dyn Driver,
    target: &Path,
    run: &str,
    arguments: &Arguments,
    report: &mut Report,
    cancel: &workflow_graph::CancellationToken,
    conda: bool,
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
    let mut execution = Engine::new(&mut simulator)
        .cancel(cancel.clone())
        .run(&workflow, driver);
    execution.graph.simulate();
    // The simulator made up its files in the scratch folder, which the check leaves out.
    report.tasks(
        &execution.graph,
        arguments.default_image.as_deref(),
        &scratch,
        conda,
    );
    if driver.language() == Language::Cwl {
        report.operations(&execution.graph);
    }
    match &execution.error {
        // Workflow code that reads made-up outputs can fail where real ones would not, like
        // `logs[-1]` of the two reports of a tool that the simulator makes as one file.
        Some(ExecuteError::Driver(error))
            if !error.unsupported && made_outputs(&execution.graph) =>
        {
            report.add_lines(Kind::Warning, &error.message);
            report.notes.push(
                "the workflow code stopped on made-up task outputs, so a real run may go on \
                 there; the check cannot see the tasks after it"
                    .into(),
            );
        }
        Some(error) => report.execute_error(error),
        None => {}
    }
    let _ = std::fs::remove_dir_all(&simulated);
    Some(execution.graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_problem_wins() {
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
    fn preserves_message_continuations() {
        let mut report = Report::default();
        let error = DriverError::new(
            "uwe: WorkflowError: Missing input files for rule copy:\n    affected files:\n",
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
    fn counts_missing_files() {
        let files: Vec<PathBuf> = ["/d/a", "/d/b", "/d/c", "/d/e"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            missing_files(&["A"], &files[..1]),
            "step A reads /d/a, which does not exist"
        );
        assert_eq!(
            missing_files(&["A", "B"], &files[..2]),
            "steps A and B read /d/a and /d/b, which do not exist"
        );
        assert_eq!(
            missing_files(&["A"], &files),
            "step A reads 4 files that do not exist, like /d/a and /d/b"
        );
    }
}
