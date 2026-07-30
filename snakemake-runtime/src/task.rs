//! Turns a job into a task: the formatted shell command or the script, wrapper or `run:`
//! block, the container image, the files it reads and writes, and its resources.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use python_evaluator::{Args, Error, Interpreter, Value};
use workflow_graph::{Input, Task, TaskId};

use crate::embedded::{self, Context, data};
use crate::files::WorkflowFiles;
use crate::functions::{call_input_function, is_callable};
use crate::objects::NamedList;
use crate::pattern;
use crate::plan::{Job, incomplete_target};
use crate::rule::{Action, Items, Rule};
use crate::shell::{command_line, format_command};
use crate::workflow::{State, workflow_error};

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

/// The variables of numerical libraries that Snakemake's shell() sets to the job's threads.
const THREAD_VARIABLES: &[&str] = &[
    "OMP_NUM_THREADS",
    "GOTO_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "MKL_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
];

/// A container image as TES expects it: `docker://` URIs lose their scheme.
pub fn image_name(text: &str) -> String {
    text.strip_prefix("docker://").unwrap_or(text).to_owned()
}

/// The path of a source file of a rule, relative to the working directory when it is inside,
/// and its absolute location: the web address for a file of a remote module.
fn source_file(rule: &Rule, path: &str, setting: &TaskSetting) -> (String, String) {
    let workdir = setting.workdir;
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
    if let Some(address) = setting.state.mirrors.address(&absolute) {
        return source_file(rule, &address, setting);
    }
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

/// A script of a rule with its text. A file of a remote module comes from its mirror, which the
/// workflow's files are read from; a simulated run reads no file on the web, and goes on
/// without a file that is not there, as Snakemake's dry run does not open scripts.
fn script_source(
    interpreter: &mut Interpreter,
    location: &str,
    setting: &TaskSetting<'_>,
) -> Result<embedded::script::Source, Error> {
    let simulated = setting.embedded.simulated;
    let local = if location.contains("://") {
        if simulated {
            None
        } else {
            setting.state.mirrors.url(location)
        }
    } else {
        Some(PathBuf::from(location))
    };
    let text = match local {
        Some(path) => match interpreter.host.read(&path) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(_) if simulated => None,
            Err(error) => {
                return Err(workflow_error(format!(
                    "Failed to open source file {location}\n{error}"
                )));
            }
        },
        None => None,
    };
    Ok(embedded::script::Source {
        location: location.to_owned(),
        text,
    })
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
    /// What the code of jobs needs: the stand-in of the snakemake package.
    pub embedded: &'a embedded::Setting,
    /// The source files of the workflow, which every job sees in its work directory.
    pub sources: &'a crate::sources::Sources,
}

/// Builds the task of a job.
pub fn build(
    interpreter: &mut Interpreter,
    setting: &TaskSetting<'_>,
    rule: &Rule,
    job: &Job,
    id: TaskId,
    attempt: u32,
) -> Result<Task, Error> {
    let wildcards = NamedList::wildcards(&job.wildcards);
    let input = plain(&job.input, "InputFiles");
    let output = plain(&job.output, "OutputFiles");
    let log = plain(&job.log, "Log");
    let attempt = Value::Int(attempt as i64);
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
    // The workflow profile may set the threads of the rule.
    let threads_value = match setting
        .state
        .profile
        .set_threads
        .iter()
        .rev()
        .find(|(name, _)| *name == rule.name)
    {
        Some((_, value)) => crate::config::parse_value(value),
        None => threads_value,
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
    let resources =
        crate::resources::evaluate(interpreter, rule, &setting.state.profile, &job_values)?;
    let resource_value = crate::resources::named_list(threads, &resources);
    let mut params = NamedList::new("Params", Vec::new());
    for (name, values, many) in rule.params.named_entries() {
        let start = params.values.len();
        for value in values {
            if value.tag("unpack").is_some() {
                return Err(workflow_error(format!(
                    "unpack() is not allowed with params. Simply return a dictionary which can be directly .used, e.g. via {{params[mykey]}}. (rule {})",
                    rule.name
                )));
            }
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
    // The values that scripts, wrappers, notebooks and `run:` blocks get.
    let context = |interpreter: &mut Interpreter| Context {
        input: named(data::files("InputFiles", &job.input)),
        output: named(data::files("OutputFiles", &job.output)),
        params: named(data::from_value(interpreter, &params)),
        wildcards: named(data::from_value(interpreter, &wildcards)),
        threads,
        resources: Context::resources(interpreter, threads, &resources),
        log: named(data::files("Log", &job.log)),
        config: data::from_value(interpreter, &setting.state.config),
        rule: rule.name.clone(),
        bench_iteration: None,
        scriptdir: String::new(),
    };
    let mut inputs: Vec<Input> = Vec::new();
    for file in job.input_files() {
        // A file in storage comes from its address.
        let address = job.input.values.iter().find_map(|value| {
            (value.as_str() == Some(file.as_str()))
                .then(|| value.tag(crate::storage::TAG)?.as_str().map(str::to_owned))
                .flatten()
        });
        if let Some(address) = address {
            inputs.push(Input {
                path: file,
                source: address,
                directory: false,
                content: None,
                ..Default::default()
            });
            continue;
        }
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
            ..Default::default()
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
            command = command_line(setting.state, &formatted);
            script = Some(formatted);
        }
        Action::Script(value) | Action::Notebook(value) => {
            let path = format(interpreter, value)?;
            let (relative, location) = source_file(rule, &path, setting);
            let source = script_source(interpreter, &location, setting)?;
            let context = context(interpreter);
            let parts = if matches!(rule.action, Action::Notebook(_)) {
                embedded::notebook::build(context, &source, setting.embedded)
                    .map(|parts| (parts.command, parts.inputs))
            } else {
                embedded::script::build(context, &source, setting.embedded, setting.workdir)
                    .map(|parts| (parts.command, parts.inputs))
            };
            match parts {
                Ok((script_command, script_inputs)) => {
                    command = script_command;
                    inputs.extend(script_inputs);
                }
                // A simulated run does not run the script, as Snakemake's dry run does not
                // make its preamble; it stages the script as it is.
                Err(_) if setting.embedded.simulated => {
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
                        source: location,
                        directory: false,
                        content: None,
                        ..Default::default()
                    });
                }
                Err(message) => return Err(workflow_error(message)),
            }
        }
        Action::Wrapper(value) => {
            let wrapper = format(interpreter, value)?;
            let simulated = setting.embedded.simulated;
            // Snakemake's dry run does not look for the wrapper's script.
            let source = match embedded::wrapper::source(
                &wrapper,
                setting.workdir,
                &setting.state.mirrors,
                simulated,
            ) {
                Ok(source) => source,
                Err(_) if simulated => embedded::script::Source {
                    location: embedded::wrapper::location(&wrapper, setting.workdir),
                    text: None,
                },
                Err(message) => return Err(workflow_error(message)),
            };
            let folder = embedded::wrapper::folder(&source.location).to_owned();
            let parts = embedded::script::build(
                context(interpreter),
                &source,
                setting.embedded,
                setting.workdir,
            );
            match parts {
                Ok(parts) => {
                    command = parts.command;
                    inputs.extend(parts.inputs);
                }
                Err(_) if simulated => {
                    command = vec!["python".into(), "wrapper.py".into()];
                    inputs.push(Input {
                        path: "wrapper.py".into(),
                        source: source.location.clone(),
                        directory: false,
                        content: None,
                        ..Default::default()
                    });
                }
                Err(message) => return Err(workflow_error(message)),
            }
            if conda.is_none() {
                conda = Some(format!("{folder}/environment.yaml"));
            }
        }
        Action::Run(text) => {
            let dedented = python_evaluator::modules::dedent(text);
            let block = embedded::run::Block {
                rule: &rule.name,
                text,
                globals: &rule.globals,
                jobid: id.0,
                basedir: &rule.basedir,
                shell: embedded::run::Shell {
                    executable: setting.state.shell_executable.as_deref(),
                    prefix: setting.state.shell_prefix.as_deref(),
                    suffix: &setting.state.shell_suffix,
                },
            };
            let context = context(interpreter);
            match embedded::run::build(
                interpreter,
                &context,
                &block,
                setting.embedded,
                setting.workdir,
            ) {
                Ok(parts) => {
                    command = parts.command;
                    inputs.extend(parts.inputs);
                }
                Err(_) if setting.embedded.simulated => {
                    command = vec!["python".into(), "-c".into(), dedented.clone()];
                }
                Err(message) => return Err(workflow_error(message)),
            }
            script = Some(dedented);
        }
        Action::TemplateEngine(value) => {
            let engine = interpreter.to_str(value)?;
            command = vec!["python".into(), "-m".into(), engine];
        }
        Action::Cwl(value) => {
            let path = format(interpreter, value)?;
            let (relative, source) = source_file(rule, &path, setting);
            command = vec!["cwltool".into(), relative.clone()];
            inputs.push(Input {
                path: relative,
                source,
                directory: false,
                content: None,
                ..Default::default()
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
        // A rule without an action is a no-op task: no command and no image.
        _ if matches!(rule.action, Action::None) => None,
        (Some(container), _) => Some(image_name(&container)),
        // A `run:` block runs in the Python image that the user gave, else in the default
        // image of the executor.
        (None, None) if matches!(rule.action, Action::Run(_)) => {
            setting.embedded.python_image.clone()
        }
        (None, Some(conda)) => {
            let conda = pattern::apply(&conda, &job.wildcards).unwrap_or(conda);
            let is_file = conda.ends_with(".yaml") || conda.ends_with(".yml");
            let reference = if is_file && !conda.contains("://") {
                source_file(rule, &conda, setting).0
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
    // Snakemake makes the folders of a job's outputs before the job runs.
    for folder in output_folders(&outputs) {
        inputs.push(Input {
            path: folder,
            source: String::new(),
            directory: true,
            content: None,
            ..Default::default()
        });
    }
    // The source files of the workflow, as Snakemake's jobs see them in the working directory.
    let taken: Vec<&str> = (inputs.iter().map(|input| input.path.as_str()))
        .chain(outputs.iter().map(String::as_str))
        .collect();
    let sources = setting.sources.inputs(&taken);
    inputs.extend(sources);
    let mut environment = BTreeMap::new();
    for name in &setting.state.envvars {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.clone(), value);
        }
    }
    // Snakemake's shell() gives every command of a job its threads for the numerical
    // libraries, and its temporary folder.
    for name in THREAD_VARIABLES {
        environment.insert((*name).to_owned(), threads.to_string());
    }
    if let Some((_, tmpdir)) = resources.iter().find(|(name, _)| name == "tmpdir")
        && let Some(tmpdir) = tmpdir.as_str()
    {
        for name in ["TMPDIR", "TMP", "TEMPDIR", "TEMP"] {
            environment.insert(name.to_owned(), tmpdir.to_owned());
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
        offline: false,
        after: Vec::new(),
    })
}

/// The folders of relative outputs, without the ones inside another of them: made folders
/// that the task gets as empty directories.
fn output_folders(outputs: &[String]) -> Vec<String> {
    let mut folders: Vec<String> = outputs
        .iter()
        .filter(|output| !output.starts_with('/'))
        .filter_map(|output| {
            let parent = Path::new(output.trim_end_matches('/')).parent()?;
            let parent = parent.to_string_lossy().into_owned();
            (!parent.is_empty() && !parent.split('/').any(|part| part == "..")).then_some(parent)
        })
        .collect();
    folders.sort();
    folders.dedup();
    let mut kept: Vec<String> = Vec::new();
    for folder in folders.into_iter().rev() {
        if !kept
            .iter()
            .any(|inner| inner.starts_with(&format!("{folder}/")))
        {
            kept.push(folder);
        }
    }
    kept.reverse();
    kept
}

/// A value of the context that is a `Namedlist`; other values become a list of one item.
fn named(data: data::Data) -> data::Named {
    match data {
        data::Data::Named(named) => named,
        other => data::Named::new("Namedlist", vec![other]),
    }
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
