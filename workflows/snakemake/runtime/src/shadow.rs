//! Stages shallow directory links or full file links while keeping shadow outputs isolated.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use python_evaluator::Error;
use workflow_graph::Input;

use crate::shell::quote;
use crate::task::TaskSetting;
use crate::workflow::workflow_error;

pub fn stage(
    setting: &TaskSetting<'_>,
    mode: &str,
    inputs: &mut Vec<Input>,
    outputs: &[String],
    command: &mut Vec<String>,
) -> Result<(), Error> {
    for output in outputs {
        if Path::new(output).is_absolute() || output.split('/').any(|part| part == "..") {
            return Err(workflow_error(format!(
                "Shadow output {output} must stay inside the task directory"
            )));
        }
        if inputs.iter().any(|input| {
            input.directory
                && !input.source.is_empty()
                && Path::new(output).starts_with(&input.path)
        }) {
            return Err(Error::unsupported(format!(
                "Shadow output {output} overlaps a linked directory input and would escape the task directory"
            )));
        }
    }
    let mut setup = String::new();
    walk(
        setting,
        setting.workdir,
        mode,
        inputs,
        outputs,
        &mut HashSet::new(),
        &mut setup,
    )?;
    if !setup.is_empty() && !command.is_empty() {
        setup.push_str("exec ");
        setup.push_str(
            &command
                .iter()
                .map(|part| quote(part))
                .collect::<Vec<_>>()
                .join(" "),
        );
        *command = vec!["sh".into(), "-ec".into(), setup];
    }
    Ok(())
}

fn walk(
    setting: &TaskSetting<'_>,
    folder: &Path,
    mode: &str,
    inputs: &mut Vec<Input>,
    outputs: &[String],
    visited: &mut HashSet<PathBuf>,
    setup: &mut String,
) -> Result<(), Error> {
    let real = std::fs::canonicalize(folder)
        .map_err(|error| Error::os_error(&error, &folder.to_string_lossy()))?;
    if !visited.insert(real.clone()) {
        return Err(workflow_error("Shadow directory contains a symlink cycle"));
    }
    let entries = std::fs::read_dir(folder)
        .map_err(|error| Error::os_error(&error, &folder.to_string_lossy()))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::os_error(&error, &folder.to_string_lossy()))?;
        let source = entry.path();
        let path = source.strip_prefix(setting.workdir).unwrap_or(&source);
        if path.starts_with(".snakemake") || source.starts_with(setting.embedded.scratch()) {
            continue;
        }
        let directory = source.is_dir();
        let output = outputs
            .iter()
            .any(|output| path.starts_with(output.trim_end_matches('/')));
        if output {
            continue;
        }
        let ancestor = outputs
            .iter()
            .any(|output| Path::new(output).starts_with(path));
        if directory && mode == "shallow" && ancestor {
            return Err(Error::unsupported(format!(
                "Shallow shadow output inside {} would escape the task directory through a directory symlink; use full shadow",
                path.display()
            )));
        }
        let text = path.to_string_lossy().into_owned();
        let taken = inputs.iter().any(|input| Path::new(&input.path) == path);
        if directory && mode == "full" {
            if !taken {
                inputs.push(Input {
                    path: text,
                    directory: true,
                    ..Input::default()
                });
            }
            walk(setting, &source, mode, inputs, outputs, visited, setup)?;
        } else if directory && mode == "shallow" {
            let absolute = source.to_string_lossy().into_owned();
            inputs.push(Input {
                path: absolute.clone(),
                source: absolute.clone(),
                directory: true,
                ..Input::default()
            });
            setup.push_str(&format!(
                "rm -rf -- {path}; ln -s -- {source} {path};\n",
                path = quote(&text),
                source = quote(&absolute)
            ));
        } else if !taken {
            inputs.push(Input {
                path: text,
                source: source.to_string_lossy().into_owned(),
                directory,
                ..Input::default()
            });
        }
    }
    visited.remove(&real);
    Ok(())
}
