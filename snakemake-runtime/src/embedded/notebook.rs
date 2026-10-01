//! Prepends notebook context and selects papermill or jupyter-nbconvert to run it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use workflow_graph::Input;

use super::script::Source;
use super::{Context, SCRIPTS_FOLDER, Setting, literal, python, r};

/// The tag of the preamble cell.
const TAG: &str = "snakemake-job-properties";

/// The kernel languages of notebooks that Snakemake runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kernel {
    Python,
    R,
}

/// What a notebook adds to its task.
pub struct Parts {
    pub command: Vec<String>,
    pub inputs: Vec<Input>,
}

fn error(message: impl Into<String>) -> String {
    message.into()
}

/// The language of a notebook's kernel, from its metadata.
fn kernel(notebook: &Json) -> Result<Kernel, String> {
    let name = notebook
        .pointer("/metadata/language_info/name")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            error(
                "Notebook metadata is corrupt. Please delete notebook and recreate it via \
                 --edit-notebook.",
            )
        })?;
    match name.to_ascii_lowercase().as_str() {
        "python" => Ok(Kernel::Python),
        "r" => Ok(Kernel::R),
        _ => Err(error(
            "Unsupported notebook: Expecting Jupyter Notebook (.ipynb).",
        )),
    }
}

/// The notebook with the preamble as its first cell; an earlier preamble cell goes, as
/// Snakemake replaces it.
fn with_preamble(mut notebook: Json, preamble: &str) -> Result<Json, String> {
    let cells = notebook
        .get_mut("cells")
        .and_then(Json::as_array_mut)
        .ok_or_else(|| error("the notebook has no cells"))?;
    let tagged = |cell: &Json| {
        cell.pointer("/metadata/tags")
            .and_then(Json::as_array)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(TAG)))
    };
    match cells.iter().filter(|cell| tagged(cell)).count() {
        0 => {}
        1 => cells.retain(|cell| !tagged(cell)),
        _ => {
            return Err(error(
                "More than one snakemake preamble cell found in notebook. Please clean up the \
                 notebook first, by removing all or all but one of them.",
            ));
        }
    }
    let mut metadata = Map::new();
    metadata.insert("tags".into(), json!([TAG]));
    cells.insert(
        0,
        json!({
            "cell_type": "code",
            "execution_count": null,
            "id": TAG,
            "metadata": metadata,
            "outputs": [],
            "source": preamble,
        }),
    );
    Ok(notebook)
}

/// Code that moves a kernel that started in the folder of the notebook back to the working
/// directory of the job, as Snakemake's preamble moves it to its own working directory.
const PYTHON_WORKDIR: &str = "import os; os.getcwd().endswith('/.snakemake/scripts') and \
    os.chdir(os.path.dirname(os.path.dirname(os.getcwd()))); ";
const R_WORKDIR: &str =
    "if (endsWith(getwd(), \"/.snakemake/scripts\")) setwd(dirname(dirname(getwd())));";

/// The command that runs a notebook as Snakemake does: papermill if the image has it, else
/// `jupyter-nbconvert`, with the executed notebook written to the log named `notebook`.
fn command(path: &str, executed: Option<&str>) -> Vec<String> {
    let papermill_output = executed.unwrap_or(path);
    let nbconvert_output = match executed {
        Some(executed) if Path::new(executed).is_absolute() => format!("--output '{executed}'"),
        Some(executed) => format!("--output \"$PWD\"/'{executed}'"),
        None => "--output \"$IPYTHONDIR/notebook.ipynb\"".to_owned(),
    };
    // A container that runs as the user of the run often has `/` as its home, where Jupyter
    // cannot keep its settings; it then gets the temporary folder of IPython as its home.
    let script = format!(
        "set -euo pipefail; export IPYTHONDIR=\"$(mktemp -d)\"; \
         if [ ! -w \"${{HOME:-/}}\" ]; then export HOME=\"$IPYTHONDIR\"; fi; \
         if papermill --version > /dev/null 2>&1; then \
         papermill --log-level ERROR '{path}' '{papermill_output}'; \
         else jupyter-nbconvert --log-level ERROR --execute {nbconvert_output} --to notebook \
         --ExecutePreprocessor.timeout=-1 '{path}'; fi"
    );
    vec!["bash".into(), "-c".into(), script]
}

/// The task parts of a notebook.
pub fn build(mut context: Context, source: &Source, setting: &Setting) -> Result<Parts, String> {
    let name = source.file_name().to_owned();
    if !name.ends_with(".ipynb") {
        return Err(error(
            "Unsupported notebook: Expecting Jupyter Notebook (.ipynb).",
        ));
    }
    context.scriptdir = source.folder();
    let path = format!("{SCRIPTS_FOLDER}/{name}");
    let executed = context.log.named("notebook").map(|log| log.text());
    let Some(text) = &source.text else {
        // A notebook on the web that a simulated run did not read: a Python kernel.
        return Ok(Parts {
            command: command(&path, executed.as_deref()),
            inputs: vec![
                literal(
                    &path,
                    &format!(
                        "iunctus reads {} when the job runs for real",
                        source.location
                    ),
                ),
                setting.package()?,
            ],
        });
    };
    let notebook: Json = serde_json::from_str(text)
        .map_err(|problem| format!("the notebook {} is no JSON: {problem}", source.location))?;
    let kernel = kernel(&notebook)?;
    let preamble = match kernel {
        Kernel::Python => {
            let searchpaths = if source.is_local() {
                vec![context.scriptdir.clone()]
            } else {
                Vec::new()
            };
            python::cell(&context, &searchpaths, PYTHON_WORKDIR)
        }
        Kernel::R => r::object(&context, Some(R_WORKDIR))?,
    };
    let notebook = with_preamble(notebook, &preamble)?;
    let text = serde_json::to_string_pretty(&notebook)
        .map_err(|problem| format!("cannot write the notebook: {problem}"))?;
    let mut inputs = vec![literal(&path, &format!("{text}\n"))];
    if kernel == Kernel::Python {
        inputs.push(setting.package()?);
    }
    Ok(Parts {
        command: command(&path, executed.as_deref()),
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{TAG, with_preamble};

    #[test]
    fn replaces_notebook_preamble() {
        let notebook = json!({
            "cells": [
                {"cell_type": "code", "metadata": {"tags": [TAG]}, "source": "old"},
                {"cell_type": "code", "metadata": {}, "source": "print(1)"},
            ],
            "metadata": {"language_info": {"name": "python"}},
        });
        let notebook = with_preamble(notebook, "new").expect("a notebook");
        let cells = notebook["cells"].as_array().expect("cells");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["source"], "new");
        assert_eq!(cells[1]["source"], "print(1)");
    }
}
