//! The source files of a workflow, which every job sees at their paths in its work directory,
//! as Snakemake's jobs see them in the working directory.
//!
//! A task sees only the files that it gets (decision 11). Snakemake runs jobs in the working
//! directory, where shell commands also call scripts and packages of the workflow that their
//! rule does not declare. So every task also gets the files that Snakemake deploys to jobs
//! that run without a shared file system (`DAG.get_sources` of Snakemake 9): the files that
//! git tracks below the working directory, the folders of the rules' scripts and notebooks,
//! the conda files of the rules and the config files, each at most 10 MB and below the working
//! directory. The Snakefiles are left out: a job runs the code of its own rule, which its task
//! has already. Each source is a read-only input with its digest in the cache key, so the
//! result of a task stays tied to what it could read.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use workflow_graph::Input;

use crate::rule::Action;
use crate::workflow::State;

/// The largest source file that Snakemake deploys to its jobs.
const LARGEST: u64 = 10_000_000;

/// The source files of a workflow, by their paths relative to the working directory.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    files: BTreeMap<String, PathBuf>,
}

impl Sources {
    /// The source files of the workflow that `state` evaluated.
    pub fn find(state: &State) -> Sources {
        let Ok(workdir) = std::fs::canonicalize(&state.workdir) else {
            return Sources::default();
        };
        let mut found: Vec<PathBuf> = crate::git::tracked(&workdir).unwrap_or_default();
        for rule in &state.rules {
            let script = match &rule.action {
                Action::Script(value) | Action::Notebook(value) => value.as_str(),
                _ => None,
            };
            // The script, and the files below its folder unless that is the working directory
            // itself, as Snakemake walks the folder of the script's path relative to it. A
            // path with wildcards names its folder as well as a plain one.
            if let Some(file) = script.and_then(|path| script_file(&rule.basedir, path)) {
                if let Some(folder) = file.parent()
                    && let Ok(folder) = std::fs::canonicalize(folder)
                    && folder != workdir
                {
                    files_below(&folder, &mut found);
                }
                found.push(file);
            }
            let conda = rule.conda.as_ref().and_then(|value| value.as_str());
            if let Some(conda) =
                conda.filter(|file| file.ends_with(".yaml") || file.ends_with(".yml"))
                && !conda.contains("://")
            {
                found.push(rule.basedir.join(conda));
            }
        }
        found.extend(
            state
                .configfiles
                .iter()
                .map(|file| state.workdir.join(file)),
        );
        let snakefiles: Vec<PathBuf> = (state.included.iter())
            .chain([&state.main_snakefile])
            .filter_map(|file| std::fs::canonicalize(file).ok())
            .collect();
        let mut files = BTreeMap::new();
        for file in found {
            let Some(place) = place_of(&file) else {
                continue;
            };
            // The size of what a link points to, as Snakemake measures it.
            let Ok(metadata) = std::fs::metadata(&place) else {
                continue;
            };
            let snakefile =
                std::fs::canonicalize(&place).is_ok_and(|real| snakefiles.contains(&real));
            if !metadata.is_file() || metadata.len() > LARGEST || snakefile {
                continue;
            }
            if let Some(relative) = relative_to(&place, &workdir) {
                files.insert(relative, place);
            }
        }
        Sources { files }
    }

    /// The sources as inputs of a job whose own inputs and outputs are at `taken`: a source
    /// at such a path, inside one or around one gives way to the job's own file.
    pub fn inputs(&self, taken: &[&str]) -> Vec<Input> {
        let taken: Vec<&str> = (taken.iter())
            .map(|path| path.trim_start_matches("./").trim_end_matches('/'))
            .filter(|path| !path.starts_with('/'))
            .collect();
        let clashes = |path: &str| {
            taken
                .iter()
                .any(|own| path == *own || inside(path, own) || inside(own, path) || own.is_empty())
        };
        (self.files.iter())
            .filter(|(path, _)| !clashes(path))
            .map(|(path, source)| Input {
                path: path.clone(),
                source: source.to_string_lossy().into_owned(),
                ..Input::default()
            })
            .collect()
    }
}

/// Whether `path` lies inside the folder `folder`.
fn inside(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The local file of a script of a rule whose Snakefile is in `basedir`.
fn script_file(basedir: &Path, script: &str) -> Option<PathBuf> {
    if script.contains("://") {
        return None;
    }
    Some(python_evaluator::host::normalize(&basedir.join(script)))
}

/// Adds the files below a folder, as Snakemake walks the folder of a script.
fn files_below(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files_below(&path, found),
            Ok(_) => found.push(path),
            Err(_) => {}
        }
    }
}

/// Where a file is, with the links of its folders resolved but not a link that it is itself,
/// since git tracks the link.
fn place_of(file: &Path) -> Option<PathBuf> {
    let parent = std::fs::canonicalize(file.parent()?).ok()?;
    Some(parent.join(file.file_name()?))
}

/// A path relative to the working directory, when it lies below it.
fn relative_to(path: &Path, workdir: &Path) -> Option<String> {
    let relative = path.strip_prefix(workdir).ok()?;
    let text = relative.to_str()?;
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(files: &[&str]) -> Sources {
        Sources {
            files: (files.iter())
                .map(|path| ((*path).to_owned(), PathBuf::from("/w").join(path)))
                .collect(),
        }
    }

    fn paths(inputs: &[Input]) -> Vec<&str> {
        inputs.iter().map(|input| input.path.as_str()).collect()
    }

    #[test]
    fn prioritizes_job_files() {
        let sources = sources(&[
            "scripts/a.py",
            "results/.gitkeep",
            "data",
            "config/config.yaml",
            "resultsx.txt",
        ]);
        let inputs = sources.inputs(&["results/", "data/sample.txt", "config/config.yaml"]);
        assert_eq!(paths(&inputs), ["resultsx.txt", "scripts/a.py"]);
        assert_eq!(inputs[1].source, "/w/scripts/a.py");
        // An absolute output leaves the sources as they are.
        assert_eq!(sources.inputs(&["/tmp/x"]).len(), 5);
    }
}
