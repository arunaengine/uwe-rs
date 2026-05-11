//! Turns a job into a task: the formatted shell command or the script, wrapper or `run:`
//! block, the container image, the files it reads and writes, and its resources.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use python_evaluator::{Args, Error, Interpreter, Value};
use workflow_graph::{Input, Task, TaskId};

use crate::files::WorkflowFiles;
use crate::functions::{call_input_function, is_callable};
use crate::objects::NamedList;
use crate::pattern;
use crate::plan::{Job, incomplete_target};
use crate::rule::{Action, Items, Rule};
use crate::shell::format_command;
use crate::workflow::{State, workflow_error};

/// Where wrappers come from unless the workflow names a URL.
pub const WRAPPER_PREFIX: &str = "https://github.com/snakemake/snakemake-wrappers/raw/";

/// Plain texts of items, as `Namedlist._plainstrings` gives them to functions.
fn plain(items: &Items, kind: &'static str) -> Value {
    let values = items
        .values
        .iter()
        .map(|value| match value.as_str() {
            Some(text) => Value::str(text),
            None => value.clone(),
        })
        .collect();
    NamedList {
        values,
        names: items.names.clone(),
        kind,
    }
    .value()
}

/// The value of a directive that may be a function of wildcards and other job values.
pub fn evaluate(
    interpreter: &mut Interpreter,
    value: &Value,
    wildcards: &Value,
    aux: &[(&str, Value)],
) -> Result<Value, Error> {
    if is_callable(interpreter, value) {
        call_input_function(interpreter, value.plain(), wildcards, aux)
    } else {
        Ok(value.clone())
    }
}

/// A container image as TES expects it: `docker://` URIs lose their scheme.
fn image_name(text: &str) -> String {
    text.strip_prefix("docker://").unwrap_or(text).to_owned()
}

/// The path of a source file of a rule, relative to the working directory when it is inside,
/// and its absolute location.
fn source_file(rule: &Rule, path: &str, workdir: &Path) -> (String, String) {
    if path.contains("://") {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        return (name, path.to_owned());
    }
    let absolute = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        rule.basedir.join(path)
    };
    let absolute = python_evaluator::host::normalize(&absolute);
    let relative = absolute
        .strip_prefix(workdir)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            format!(
                ".snakemake/scripts/{}",
                absolute
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )
        });
    (relative, absolute.to_string_lossy().into_owned())
}

/// The program that runs a script, by its file extension, as Snakemake's script support.
fn script_command(path: &str) -> Result<Vec<String>, Error> {
    let lower = path.to_ascii_lowercase();
    let command: Vec<&str> = if lower.ends_with(".py") {
        vec!["python", path]
    } else if lower.ends_with(".r") {
        vec!["Rscript", "--vanilla", path]
    } else if lower.ends_with(".rmd") {
        return Ok(vec![
            "Rscript".into(),
            "--vanilla".into(),
            "-e".into(),
            format!("rmarkdown::render('{path}')"),
        ]);
    } else if lower.ends_with(".jl") {
        vec!["julia", path]
    } else if lower.ends_with(".sh") {
        vec!["bash", path]
    } else if lower.ends_with(".rs") {
        vec!["rust-script", path]
    } else if lower.ends_with(".xsh") {
        vec!["xonsh", path]
    } else if lower.ends_with(".hy") {
        vec!["hy", path]
    } else {
        return Err(workflow_error(format!(
            "Unsupported script: Expecting either Python (.py), R (.R), RMarkdown (.Rmd), Julia (.jl), Rust (.rs), Bash (.sh), Xonsh (.xsh) or Hy (.hy) script ({path})."
        )));
    };
    Ok(command.into_iter().map(str::to_owned).collect())
}

/// What the session knows that tasks need besides the job.
pub struct TaskSetting<'a> {
    pub state: &'a State,
    pub files: &'a WorkflowFiles,
    pub workdir: &'a Path,
}

/// Builds the task of a job.
pub fn build(
    interpreter: &mut Interpreter,
    setting: &TaskSetting<'_>,
    rule: &Rule,
    job: &Job,
    id: TaskId,
) -> Result<Task, Error> {
    let wildcards = NamedList::wildcards(&job.wildcards);
    let input = plain(&job.input, "InputFiles");
    let output = plain(&job.output, "OutputFiles");
    let log = plain(&job.log, "Log");
    let attempt = Value::Int(1);
    let rulename = Value::str(rule.name.as_str());
    let threads_value = match &rule.threads {
        Some(value) => evaluate(
            interpreter,
            value,
            &wildcards,
            &[
                ("input", input.clone()),
                ("attempt", attempt.clone()),
                ("rulename", rulename.clone()),
            ],
        )?,
        None => Value::Int(1),
    };
    let threads = match threads_value.plain() {
        Value::Float(value) => *value as i64,
        other => other.as_int().ok_or_else(|| {
            workflow_error(format!(
                "Threads must be given as an int (rule {})",
                rule.name
            ))
        })?,
    };
    let job_values = crate::resources::JobValues {
        wildcards: &wildcards,
        input: &input,
        attempt: &attempt,
        threads,
        rulename: &rulename,
    };
    let resources = crate::resources::evaluate(interpreter, rule, &job_values)?;
    let resource_value = crate::resources::named_list(threads, &resources);
    let mut params = NamedList::new("Params", Vec::new());
    for (name, values, many) in rule.params.named_entries() {
        let start = params.values.len();
        for value in values {
            let concrete = if is_callable(interpreter, &value) {
                match call_input_function(
                    interpreter,
                    value.plain(),
                    &wildcards,
                    &[
                        ("input", input.clone()),
                        ("output", output.clone()),
                        ("threads", Value::Int(threads)),
                        ("resources", resource_value.clone()),
                    ],
                ) {
                    Ok(result) => result,
                    Err(error) if incomplete_target(&error).is_some() => Value::str("<TBD>"),
                    Err(error) => {
                        let values: Vec<String> = job
                            .wildcards
                            .iter()
                            .map(|(name, value)| format!("{name}={value}"))
                            .collect();
                        let mut wrapped = workflow_error(format!(
                            "InputFunctionException in rule {} (wildcards: {}): {}: {}",
                            rule.name,
                            values.join(", "),
                            error.class_name(),
                            error.message()
                        ));
                        wrapped.location = error.location.clone();
                        return Err(if error.is_unsupported() {
                            error
                        } else {
                            wrapped
                        });
                    }
                }
            } else {
                apply_to_param(interpreter, &value, &job.wildcards)?
            };
            params.values.push(concrete);
        }
        if let Some(name) = name {
            let end = params.values.len();
            params
                .names
                .push((name, start, if many { Some(end) } else { None }));
        }
    }
    let params = params.value();
    let names = Args {
        positional: Vec::new(),
        keywords: vec![
            ("input".into(), input.clone()),
            ("output".into(), output.clone()),
            ("params".into(), params.clone()),
            ("wildcards".into(), wildcards.clone()),
            ("threads".into(), Value::Int(threads)),
            ("resources".into(), resource_value.clone()),
            ("log".into(), log.clone()),
            ("jobid".into(), Value::Int(id.0 as i64)),
            ("name".into(), rulename.clone()),
            ("rule".into(), rulename.clone()),
            ("rulename".into(), rulename.clone()),
            ("bench_iteration".into(), Value::None),
        ],
    };
    let mut inputs: Vec<Input> = Vec::new();
    for file in job.input_files() {
        let path = interpreter.host.resolve(&file);
        let (source, directory) = match setting.files.location(&path) {
            Some((location, directory)) => (location, directory),
            None => {
                let directory = interpreter
                    .host
                    .stat(&path)
                    .is_some_and(|stat| stat.kind == python_evaluator::FileKind::Directory);
                (path, directory)
            }
        };
        inputs.push(Input {
            path: file,
            source: source.to_string_lossy().into_owned(),
            directory,
            content: None,
        });
    }
    let mut conda = match &rule.conda {
        Some(value) => {
            let value = evaluate(
                interpreter,
                value,
                &wildcards,
                &[("params", params.clone()), ("input", input.clone())],
            )?;
            if value.is_none() {
                None
            } else {
                Some(interpreter.to_str(&value)?)
            }
        }
        None => None,
    };
    let mut command: Vec<String> = Vec::new();
    let mut script: Option<String> = None;
    let format = |interpreter: &mut Interpreter, value: &Value| -> Result<String, Error> {
        let text = interpreter.to_str(value)?;
        if text.contains('{') {
            format_command(interpreter, &text, &names, Some(&rule.globals))
        } else {
            Ok(text)
        }
    };
    match &rule.action {
        Action::None => {}
        Action::Shell(value) => {
            let template = interpreter.to_str(value)?;
            let formatted = format_command(interpreter, &template, &names, Some(&rule.globals))
                .map_err(|error| shell_error(error, rule))?;
            let executable = setting
                .state
                .shell_executable
                .clone()
                .unwrap_or_else(|| "bash".into());
            let bash = Path::new(&executable)
                .file_name()
                .is_some_and(|name| name == "bash");
            let prefix = setting.state.shell_prefix.clone().unwrap_or_else(|| {
                if bash {
                    "set -euo pipefail; ".into()
                } else {
                    String::new()
                }
            });
            let full = format!("{prefix} {formatted} {}", setting.state.shell_suffix)
                .trim()
                .to_owned();
            command = vec![executable, "-c".into(), full];
            script = Some(formatted);
        }
        Action::Script(value) | Action::Notebook(value) => {
            let path = format(interpreter, value)?;
            let (relative, source) = source_file(rule, &path, setting.workdir);
            command = if matches!(rule.action, Action::Notebook(_)) {
                vec![
                    "jupyter".into(),
                    "nbconvert".into(),
                    "--execute".into(),
                    "--to".into(),
                    "notebook".into(),
                    relative.clone(),
                ]
            } else {
                script_command(&relative)?
            };
            inputs.push(Input {
                path: relative,
                source,
                directory: false,
                content: None,
            });
        }
        Action::Wrapper(value) => {
            let wrapper = format(interpreter, value)?;
            let base = if wrapper.contains("://") || wrapper.starts_with("file:") {
                wrapper.clone()
            } else {
                format!("{WRAPPER_PREFIX}{wrapper}")
            };
            let base = base.trim_end_matches('/').to_owned();
            inputs.push(Input {
                path: "wrapper.py".into(),
                source: format!("{base}/wrapper.py"),
                directory: false,
                content: None,
            });
            command = vec!["python".into(), "wrapper.py".into()];
            if conda.is_none() {
                conda = Some(format!("{base}/environment.yaml"));
            }
        }
        Action::Run(text) => {
            command = vec!["python".into(), "-c".into(), text.clone()];
            script = Some(text.clone());
        }
        Action::TemplateEngine(value) => {
            let engine = interpreter.to_str(value)?;
            command = vec!["python".into(), "-m".into(), engine];
        }
        Action::Cwl(value) => {
            let path = format(interpreter, value)?;
            let (relative, source) = source_file(rule, &path, setting.workdir);
            command = vec!["cwltool".into(), relative.clone()];
            inputs.push(Input {
                path: relative,
                source,
                directory: false,
                content: None,
            });
        }
    }
    let container = match &rule.container {
        Some(Some(value)) => {
            let value = evaluate(interpreter, value, &wildcards, &[])?;
            if value.is_none() {
                None
            } else {
                let text = interpreter.to_str(&value)?;
                Some(pattern::apply(&text, &job.wildcards).unwrap_or(text))
            }
        }
        _ => None,
    };
    let image = match (container, conda) {
        (Some(container), _) => Some(image_name(&container)),
        (None, Some(conda)) => {
            let conda = pattern::apply(&conda, &job.wildcards).unwrap_or(conda);
            let is_file = conda.ends_with(".yaml") || conda.ends_with(".yml");
            let reference = if is_file && !conda.contains("://") {
                source_file(rule, &conda, setting.workdir).0
            } else {
                conda
            };
            Some(format!("conda-env:{reference}"))
        }
        (None, None) => None,
    };
    let mut outputs: Vec<String> = Vec::new();
    for value in job.output.values.iter().chain(job.log.values.iter()) {
        if let Some(text) = value.as_str() {
            let directory = value.tag("directory").is_some();
            outputs.push(if directory {
                format!("{}/", text.trim_end_matches('/'))
            } else {
                text.to_owned()
            });
        }
    }
    if let Some(benchmark) = job.benchmark.as_ref().and_then(Value::as_str) {
        outputs.push(benchmark.to_owned());
    }
    let mut environment = BTreeMap::new();
    for name in &setting.state.envvars {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.clone(), value);
        }
    }
    let label = if job.wildcards.is_empty() {
        rule.name.clone()
    } else {
        let values: Vec<String> = job
            .wildcards
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        format!("{} ({})", rule.name, values.join(", "))
    };
    Ok(Task {
        id,
        name: label,
        step: rule.name.clone(),
        image,
        command,
        script,
        workdir: None,
        environment,
        inputs,
        outputs,
        stdin: None,
        resources: crate::resources::task_resources(threads, &resources),
        after: Vec::new(),
    })
}

/// A param that is not a function: texts and lists of texts get their wildcards filled in.
fn apply_to_param(
    interpreter: &mut Interpreter,
    value: &Value,
    wildcards: &pattern::Wildcards,
) -> Result<Value, Error> {
    let fill = |text: &str| -> Result<Value, Error> {
        pattern::apply(text, wildcards).map(Value::str).map_err(|name| {
            workflow_error(format!(
                "Wildcards in params cannot be determined from output files. Note that you have to use a function to deactivate automatic wildcard expansion in params strings, e.g., `lambda wildcards: '{{test}}'`. Also see https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html#non-file-parameters-for-rules: '{name}'"
            ))
        })
    };
    Ok(match value.plain() {
        Value::Str(text) => fill(text)?,
        Value::List(items) => {
            let items = items.borrow().clone();
            let mut filled = Vec::with_capacity(items.len());
            for item in items {
                filled.push(match item.as_str() {
                    Some(text) => fill(text)?,
                    None => item,
                });
            }
            Value::list(filled)
        }
        _ => {
            let _ = interpreter;
            value.clone()
        }
    })
}

fn shell_error(error: Error, rule: &Rule) -> Error {
    if error.is_unsupported() {
        return error;
    }
    let mut wrapped = workflow_error(format!(
        "RuleException in rule {} in file {}, line {}: {}: {}, when formatting the shell command",
        rule.name,
        rule.snakefile,
        rule.line,
        error.class_name(),
        error.message()
    ));
    wrapped.location = error.location;
    wrapped
}
