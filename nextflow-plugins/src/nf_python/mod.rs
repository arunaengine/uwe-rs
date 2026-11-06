//! nf-python 0.1.4: `pyFunction` runs Python code in a container, one fresh process per call,
//! and gives back the values that the code hands to `nf.output`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod container;
mod exchange;

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;
use sha2::{Digest, Sha256};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::entries;
use crate::nf_sqldb::{split_options, thrown};
use container::{Call, Ended};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["pyFunction"],
    call: Some(call),
    scopes: &["nf_python"],
    ..Plugin::new("nf-python", Kind::Functions, State::Partial)
};

/// The image that runs the calls when `nf_python.container` names none: the Python image
/// that uwe's Snakemake support uses too.
pub(crate) const IMAGE: &str = "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim";

/// uwe's `nf_python` module, which the calls import.
const MODULE: &str = include_str!("module.py");

/// The named arguments that set up the call rather than go to Python.
const RESERVED: [&str; 3] = ["script", "_executable", "_conda_env"];

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match name {
        "pyFunction" => py_function(arguments, context),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-python`"),
            span: Default::default(),
        }),
    }
}

/// A text setting, which must be a string.
fn setting(value: Option<Value>) -> Result<Option<String>, Error> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text) | Value::GString(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(thrown(format!("Expected a string, got: {other}"))),
    }
}

/// The Python program, from the call or else the config; uwe runs it inside the container.
fn program(options: &[(String, Value)], context: &Context) -> Result<String, Error> {
    let option = |name: &str| crate::nf_sqldb::option(options, name);
    let (executable, conda) = match option("_executable").or(option("_conda_env")) {
        Some(_) => (option("_executable"), option("_conda_env")),
        None => (
            context.config.get(&["nf_python", "executable"]),
            context.config.get(&["nf_python", "conda_env"]),
        ),
    };
    let (executable, conda) = (setting(executable)?, setting(conda)?);
    if executable.is_some() && conda.is_some() {
        return Err(thrown(
            "The 'executable' and 'conda_env' options cannot be used together",
        ));
    }
    if let Some(conda) = conda {
        return Err(thrown(format!(
            "uwe runs nf-python in a container and cannot use the conda environment {conda}; \
             set nf_python.container to an image with its packages instead"
        )));
    }
    Ok(executable.unwrap_or("python".to_owned()))
}

/// `pyFunction(code, [name: value, ...])` or `pyFunction(script: 'file.py', ...)`.
fn py_function(arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (options, positional) = split_options(arguments);
    let code = (positional.into_iter())
        .find(|value| !matches!(value, Value::Null))
        .map(|value| value.to_string())
        .filter(|code| !code.is_empty());
    let script = crate::nf_sqldb::option(&options, "script").map(|value| value.to_string());
    let program = program(&options, context)?;
    let launch = PathBuf::from(context.host.launch_dir());
    let (source, file) = match (&code, &script) {
        (Some(_), Some(_)) => {
            return Err(thrown("Cannot use both code and script options together"));
        }
        (None, None) => return Err(thrown("Missing code argument")),
        (Some(code), None) => (code.clone(), None),
        (None, Some(script)) => {
            let file = launch.join(script);
            let text = std::fs::read_to_string(&file).map_err(|error| {
                thrown(format!(
                    "cannot read the script {}: {error}",
                    file.display()
                ))
            })?;
            (text, Some(file))
        }
    };
    let passed: Vec<(String, Value)> = (options.into_iter())
        .filter(|(name, _)| !RESERVED.contains(&name.as_str()))
        .collect();
    let encoded = exchange::encode_map(&passed).map_err(thrown)?.to_string();
    let base = work_dir(context, &launch).join(".nf-python");
    let hash = Sha256::digest(format!("{program}\0{source}\0{encoded}"));
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    let folder = base.join(&hex[..2]).join(&hex[2..32]);
    let failed =
        |error: std::io::Error| thrown(format!("cannot prepare {}: {error}", folder.display()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(failed)?;
    let library = base.join("lib");
    std::fs::create_dir_all(library.join("nf_python")).map_err(failed)?;
    std::fs::write(library.join("nf_python/__init__.py"), MODULE).map_err(failed)?;
    let script = match file {
        Some(file) => file,
        None => {
            let path = folder.join("script.py");
            let text = format!("\nfrom nf_python import nf\n\n{}", dedent(&source));
            std::fs::write(&path, text).map_err(failed)?;
            path
        }
    };
    std::fs::write(folder.join("in.json"), &encoded).map_err(failed)?;
    let mut folders = vec![launch.clone()];
    for extra in [&base, script.parent().unwrap_or(&launch)] {
        if !extra.starts_with(&launch) && !folders.iter().any(|known| extra.starts_with(known)) {
            folders.push(extra.to_owned());
        }
    }
    let text = |path: PathBuf| path.to_string_lossy().into_owned();
    let call = Call {
        engine: context.host.environment("UWE_ENGINE").unwrap_or_default(),
        image: setting(context.config.get(&["nf_python", "container"]))?
            .unwrap_or(IMAGE.to_owned()),
        program: program.clone(),
        arguments: vec![text(script.clone())],
        folders,
        workdir: launch.clone(),
        environment: vec![
            ("NEXTFLOW_INFILE".to_owned(), text(folder.join("in.json"))),
            ("NEXTFLOW_OUTFILE".to_owned(), text(folder.join("out.json"))),
            ("PYTHONPATH".to_owned(), text(library)),
        ],
        stdout: folder.join("stdout.log"),
        stderr: folder.join("stderr.log"),
        status: folder.join(".exitcode"),
    };
    let ended = container::run(&call)
        .map_err(|message| thrown(format!("nf-python could not run Python: {message}")))?;
    let report = |first: &str, code: i32| report(first, &program, &script, code, &folder);
    let output = folder.join("out.json");
    let first = match ended {
        Ended::Missing => {
            return Err(thrown(format!(
                "Cannot run program \"{program}\": Exec failed, error: 2 (No such file or \
                 directory)"
            )));
        }
        Ended::Exited(0) if output.exists() => {
            let text = std::fs::read_to_string(&output)
                .map_err(|error| thrown(format!("cannot read {}: {error}", output.display())))?;
            let json = serde_json::from_str(&text).map_err(|error| thrown(error.to_string()))?;
            return exchange::decode(&json, &launch.to_string_lossy()).map_err(thrown);
        }
        Ended::Exited(0) => ("Python script did not produce expected output file.", 0),
        Ended::Exited(code) => ("Python script evaluation failed", code),
    };
    (context.host).print(&format!("ERROR ~ {}\n", report(first.0, first.1)));
    Err(thrown(first.0))
}

/// The work directory of the run: `workDir` of the config, else `workflow.workDir`.
pub(crate) fn work_dir(context: &Context, launch: &Path) -> PathBuf {
    if let Some(folder) = context
        .config
        .get(&["workDir"])
        .filter(|value| value.truth())
    {
        return launch.join(folder.to_string());
    }
    let found = entries(&context.workflow).into_iter();
    let found = found.filter(|(name, _)| name == "workDir");
    let found = found.map(|(_, value)| value.to_string()).next_back();
    launch.join(found.unwrap_or("work".to_owned()))
}

/// The code without the indent of its first line that is not blank, cut from the lines that
/// start with it; other lines stay as they are.
fn dedent(code: &str) -> String {
    let first = code
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let indent = &first[..first.len() - first.trim_start_matches([' ', '\t']).len()];
    let lines: Vec<&str> = (code.split('\n'))
        .map(|line| line.strip_prefix(indent).unwrap_or(line))
        .collect();
    lines.join("\n")
}

/// The error report of a call: the command, the first and last lines it wrote to standard
/// error, and its log files.
fn report(first: &str, program: &str, script: &Path, code: i32, folder: &Path) -> String {
    let stderr = std::fs::read_to_string(folder.join("stderr.log")).unwrap_or_default();
    let lines: Vec<&str> = stderr.lines().collect();
    let head = lines[..lines.len().min(10)].join("\n");
    let tail = lines[lines.len().saturating_sub(10)..].join("\n");
    let folder = folder.display();
    format!(
        "{first}\nCommand: {program} {} exited with exit code {code}\nFirst 10 lines of \
         stderr:\n{head}\nLast 10 lines of stderr:\n{tail}\nCheck the log files in:\n\t\
         '{folder}/stdout.log'\n\t'{folder}/stderr.log'",
        script.display()
    )
}
