//! The support report of a workflow: which parts uwe compiles into wir, runs as native blocks
//! or refuses, and the container images they name, found without running a task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use workflow_graph::{CancellationToken, ExecutionGraph, Language};

use crate::Failure;
use crate::arguments::Arguments;
pub use crate::check::{Kind, Problem};
use crate::failure::UNSUPPORTED;
use crate::setup::Setup;

/// What uwe can run of one workflow. Its JSON form is what embedders store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub language: Language,
    /// The processes, rules, steps, tasks, calls and templates, in the order found.
    pub parts: Vec<Part>,
    /// What `uwe check` finds: unsupported features, errors and warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<Problem>,
    /// What the check could not look at, like files at remote addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// One process, rule, step, task, call or template.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub support: Support,
    /// Why the part stays native or is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The source file, from the folder of the workflow, or its address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The container images as the workflow names them, with their tag or digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// How uwe runs a part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Compiled into wir.
    Compiled,
    /// Run inside wir by a native block of its language.
    Native,
    /// Refused: a run stops with exit code 33.
    Unsupported,
}

impl Report {
    /// Whether no part and no problem is unsupported. Errors, like missing inputs, depend on
    /// the values of a run and do not count.
    pub fn runnable(&self) -> bool {
        let refused = (self.parts.iter()).any(|part| part.support == Support::Unsupported);
        !refused && !(self.problems.iter()).any(|problem| problem.kind == Kind::Unsupported)
    }

    /// Every image that a part names, once.
    pub fn images(&self) -> BTreeSet<&str> {
        let images = self.parts.iter().flat_map(|part| &part.images);
        images.map(String::as_str).collect()
    }
}

/// The report of the workflow `target`, from what `uwe check` finds and what the compiler of
/// its language keeps native.
pub(crate) fn report(
    arguments: &Arguments,
    setup: &Setup,
    (target, language): (&Path, Language),
    cancel: &CancellationToken,
) -> Report {
    // Source locations and messages start in the folder of the workflow.
    let base = match target.is_dir() {
        true => target.to_owned(),
        false => target.parent().unwrap_or(Path::new(".")).to_owned(),
    };
    let base = std::path::absolute(&base).unwrap_or(base);
    let run = crate::run_id();
    let (found, graph) = crate::check::findings(arguments, setup, (target, language), &run, cancel);
    let mut problems = found.problems;
    // The plan stops at a missing input, so it may not reach what a run refuses later.
    if language == Language::Wdl {
        for message in wdl_refusals(target, arguments, setup) {
            if !(problems.iter()).any(|problem| problem.message == message) {
                let kind = Kind::Unsupported;
                problems.push(Problem { kind, message });
            }
        }
    }
    let ops = match compiled(target, language, (arguments, setup), &base, &run) {
        Ok((document, kept)) => from_document(&document, &kept),
        Err(failure) => {
            let kind = match failure.code {
                UNSUPPORTED => Kind::Unsupported,
                _ => Kind::Error,
            };
            let message = crate::failure::without_error_numbers(&failure.message);
            let message = message.trim_start_matches("uwe: ").to_owned();
            if !(problems.iter()).any(|problem| message.ends_with(&problem.message)) {
                problems.push(Problem { kind, message });
            }
            Vec::new()
        }
    };
    let mut parts = match &graph {
        Some(graph) if !graph.nodes.is_empty() => from_steps(ops, graph),
        _ => ops,
    };
    let folder = format!("{}/", base.display());
    for problem in &mut problems {
        problem.message = problem.message.replace(&folder, "");
    }
    let shown = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    refuse(&mut parts, &problems, shown.unwrap_or_default());
    let launch = crate::launch(arguments, target.to_owned());
    let _ = std::fs::remove_dir_all(launch.scratch.join("launch").join(&run));
    Report {
        language,
        parts,
        problems,
        notes: found.notes,
    }
}

/// What a run of the WDL workflow refuses where it reaches it, found without input values.
fn wdl_refusals(target: &Path, arguments: &Arguments, setup: &Setup) -> Vec<String> {
    let options = crate::run::driver::wdl_options(arguments, setup);
    let entry = arguments.targets.first().map(String::as_str);
    let document = wdl_runtime::document::load(target, options.imports.as_ref());
    let unit = document.and_then(|document| wdl_runtime::document::entry(&document, entry));
    (unit.map(|unit| wdl_runtime::document::refusals(&unit))).unwrap_or_default()
}

/// The wir document of the workflow and its ops that stay native; a wir document is its own.
fn compiled(
    target: &Path,
    language: Language,
    given: (&Arguments, &Setup),
    base: &Path,
    run: &str,
) -> Result<(wir::Document, Vec<(String, String)>), Failure> {
    if language == Language::Wir {
        let text = std::fs::read_to_string(target)
            .map_err(|error| Failure::new(format!("uwe: {}: {error}", target.display())))?;
        let document = wir::parse(&text)
            .map_err(|error| Failure::new(format!("uwe: {}: {error}", target.display())))?;
        return Ok((document, Vec::new()));
    }
    crate::lower::compiled(target, language, given, base, run)
}

/// The task, process and native ops of every graph, by the name their source gives them.
/// Tasks without an origin are the bodies of processes.
fn from_document(document: &wir::Document, kept: &[(String, String)]) -> Vec<Part> {
    let sources: BTreeMap<&str, &str> = (document.sources.iter())
        .map(|source| (source.id.as_str(), source.location.as_str()))
        .collect();
    let mut parts: Vec<Part> = Vec::new();
    for op in document.graphs.iter().flat_map(|graph| &graph.ops) {
        let origin = op.origin.as_ref();
        let (support, images, source) = match &op.kind {
            wir::Kind::Task(task) if origin.is_some() => (
                Support::Compiled,
                task.image.iter().cloned().collect(),
                None,
            ),
            wir::Kind::Process(_) => (Support::Compiled, Vec::new(), None),
            wir::Kind::Native(native) => (Support::Native, Vec::new(), Some(&native.source)),
            _ => continue,
        };
        let at = origin.and_then(|origin| origin.at.as_deref());
        // Argo ops give the line and column of their step instead of a name.
        let position = at.filter(|at| at.chars().all(|c| c.is_ascii_digit() || c == ':'));
        let name = match (at, position) {
            (Some(at), None) => at.to_owned(),
            _ => op.path.clone(),
        };
        let source = source.or(origin.map(|origin| &origin.source));
        let file = source.and_then(|id| sources.get(id.as_str()).copied());
        let location = match (file, position) {
            (Some(file), Some(position)) => Some(format!("{file}:{position}")),
            (file, _) => file.map(str::to_owned),
        };
        let reason = (support == Support::Native)
            .then(|| native_reason(&op.path, kept))
            .flatten();
        match parts.iter_mut().find(|part| part.name == name) {
            Some(part) => merge(&mut part.images, images),
            None => parts.push(Part {
                name,
                support,
                reason,
                location,
                images,
            }),
        }
    }
    parts
}

/// Why the compiler kept the op at `path` native: its own reason, else the only one given.
fn native_reason(path: &str, kept: &[(String, String)]) -> Option<String> {
    let own = (kept.iter()).find(|(at, _)| at == path || at.ends_with(&format!("/{path}")));
    match (own, kept) {
        (Some((_, reason)), _) | (None, [(_, reason)]) => Some(reason.clone()),
        (None, _) => None,
    }
}

/// The steps of the planned run, named as runs name them, with the images their tasks name
/// and what the op of each step says; then the ops that match no step.
fn from_steps(ops: Vec<Part>, graph: &ExecutionGraph) -> Vec<Part> {
    let mut steps: Vec<(&str, Vec<String>)> = Vec::new();
    for task in graph.nodes.iter().map(|node| &node.task) {
        let sidecars = (task.containers.iter()).filter_map(|container| container.image.as_ref());
        let images: Vec<String> = task.image.iter().chain(sidecars).cloned().collect();
        match steps.iter_mut().find(|(step, _)| *step == task.step) {
            Some((_, known)) => merge(known, images),
            None => steps.push((&task.step, images)),
        }
    }
    // A step of no op runs in a native block when nothing compiles into tasks.
    let natives: Vec<usize> = (0..ops.len())
        .filter(|&index| ops[index].support == Support::Native)
        .collect();
    let all_native = !natives.is_empty() && natives.len() == ops.len();
    let mut used = vec![false; ops.len()];
    let mut parts = Vec::new();
    for (step, images) in steps {
        let own = (0..ops.len()).find(|&index| same_step(&ops[index].name, step));
        let block = || (natives.iter().copied()).find(|&index| inside(&ops[index].name, step));
        let first = all_native.then(|| natives[0]);
        let mut part = match own.or_else(block).or(first) {
            Some(index) => {
                used[index] = true;
                Part {
                    name: step.to_owned(),
                    images: ops[index].images.clone(),
                    ..ops[index].clone()
                }
            }
            None => Part {
                name: step.to_owned(),
                support: Support::Compiled,
                reason: None,
                location: None,
                images: Vec::new(),
            },
        };
        merge(&mut part.images, images);
        parts.push(part);
    }
    let unused = ops.into_iter().zip(used).filter(|(_, used)| !used);
    parts.extend(unused.map(|(op, _)| op));
    parts
}

/// Marks the parts that an unsupported problem names, else the only native part, which the
/// native runtime refuses; else the problem becomes a part named after the workflow file.
fn refuse(parts: &mut Vec<Part>, problems: &[Problem], workflow: String) {
    for problem in problems
        .iter()
        .filter(|problem| problem.kind == Kind::Unsupported)
    {
        let message = &problem.message;
        let mut named: Vec<usize> = (0..parts.len())
            .filter(|&index| names(message, &parts[index].name))
            .collect();
        let natives = (0..parts.len()).filter(|&index| parts[index].support == Support::Native);
        let natives: Vec<usize> = natives.collect();
        if named.is_empty() && natives.len() == 1 {
            named = natives;
        }
        if named.is_empty() {
            parts.push(Part {
                name: workflow.clone(),
                support: Support::Unsupported,
                reason: None,
                location: None,
                images: Vec::new(),
            });
            named.push(parts.len() - 1);
        }
        for index in named {
            let part = &mut parts[index];
            part.support = Support::Unsupported;
            part.reason = Some(message.clone());
            part.location = place(message).or(part.location.take());
        }
    }
}

/// Whether the plan step `step` is the op `name`: the same, the last part of the step after
/// `:`, `/` or `.`, or the step with a part after `/`, like the tool `say/run` of `say`.
fn same_step(name: &str, step: &str) -> bool {
    step == name || step.rsplit([':', '/', '.']).next() == Some(name) || inside(step, name)
}

/// Whether `inner` lies inside `outer`, like the step `align/sort` in the block `align`.
fn inside(outer: &str, inner: &str) -> bool {
    (inner.strip_prefix(outer)).is_some_and(|rest| rest.starts_with([':', '/']))
}

/// Whether `message` names `name` as a whole word, not as the stem of a file like `main.wdl`.
fn names(message: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    message.match_indices(name).any(|(at, _)| {
        let before = message[..at].chars().next_back();
        let after = message[at + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(|c| word(c) || c == '.')
    })
}

/// The `file:line` or `file:line:column` that starts a message, when it has one.
fn place(message: &str) -> Option<String> {
    let first = message.split_whitespace().next()?.trim_end_matches(':');
    let mut pieces = first.split(':');
    let file = pieces.next()?;
    let line = pieces.next()?;
    let numbered = !line.is_empty() && line.chars().all(|c| c.is_ascii_digit());
    (numbered && file.contains('.')).then(|| first.to_owned())
}

/// Adds the images of `more` that `images` lacks, in their order.
fn merge(images: &mut Vec<String>, more: Vec<String>) {
    for image in more {
        if !images.contains(&image) {
            images.push(image);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_named_parts() {
        assert!(names("step align: `else` is refused", "align"));
        assert!(!names("main.wdl:11: `else` clauses", "main"));
        assert!(!names("realign is refused", "align"));
        assert!(same_step("FASTQC", "NFCORE_DEMO:DEMO:FASTQC"));
        assert!(same_step("say/run", "say"));
        assert!(same_step("say", "main.say"));
        assert!(!same_step("say", "sayer"));
    }

    #[test]
    fn finds_message_places() {
        assert_eq!(place("main.wdl:11: `else`").as_deref(), Some("main.wdl:11"));
        assert_eq!(place("a.nf:3:5: x").as_deref(), Some("a.nf:3:5"));
        assert_eq!(place("step A: no image"), None);
    }
}
