//! Prepares command-line tool tasks and interprets their completed outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use cwl_parser::model::{
    Argument, Class, Dynamic, Expression, LoadListing, Process, RequirementClass, Type, Version,
    short_name,
};
use cwl_parser::source::Remote;
use workflow_graph::{Network, Outcome, Resources, Reuse, Task, TaskId};

use crate::binding::{self, Binder, Bound, Key};
use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
use crate::inputs::has_code;
use crate::javascript::helper::JavaScriptSettings;
use crate::scope::Scope;
use crate::types::Types;
use crate::value::{Map, Value};

mod collect;
mod outputs;
mod stage;
mod workdir;

pub use collect::workdir;
use outputs::{operation_output, patterns, unbound};
use stage::{InPlace, Stager, adjust};
use workdir::initial_workdir;

/// The output directory inside the container, `runtime.outdir`, and the task's workdir.
pub const OUTDIR: &str = "/var/spool/cwl";
/// The temporary directory inside the container, `runtime.tmpdir`.
pub const TMPDIR: &str = "/tmp";
/// Where input files are staged inside the container, one folder per file.
pub const STAGING: &str = "/var/lib/cwl/inputs";
/// The image of tools without `DockerRequirement`, since every task runs in a container.
pub const DEFAULT_IMAGE: &str = "debian:stable-slim";
/// The file a tool may write to give its outputs itself.
pub const OUTPUT_JSON: &str = "cwl.output.json";

/// How a run treats what executors give back.
#[derive(Clone, Debug)]
pub struct Options {
    /// Take absent files as present and empty, and give outputs that the executor cannot
    /// make (like the content of `cwl.output.json`) placeholder values of their type. This
    /// is what the simulator needs, and the default while it is the only executor.
    pub simulated: bool,
    /// The image of tools without `DockerRequirement`.
    pub image: String,
    /// On placeholder inputs, give the optional inputs of the process the run starts with
    /// placeholders too, and its flags `true`, as a job that sets every input and turns
    /// everything on; by default optional inputs are null and flags `false`.
    pub fill_optional: bool,
    /// Where files at addresses are read from while the run plans: the storage providers.
    pub remote: Remote,
    pub javascript: JavaScriptSettings,
    /// The local task engine; remote callers leave this unset.
    pub local_engine: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            simulated: true,
            image: DEFAULT_IMAGE.into(),
            fill_optional: false,
            remote: Remote::default(),
            javascript: JavaScriptSettings::default(),
            local_engine: None,
        }
    }
}

impl Options {
    pub fn access(&self) -> Access {
        Access {
            assume_present: self.simulated,
            remote: self.remote.clone(),
        }
    }
}

/// A task that runs, and what collecting its outputs needs.
#[derive(Debug)]
pub struct Pending {
    process: Arc<Process>,
    evaluator: Evaluator,
    types: Types,
    /// The input object with container paths, `inputs` of output expressions.
    inputs: Value,
    runtime: Map,
    outdir: String,
    /// The locations of staged files by their container path.
    staged: HashMap<String, (String, bool)>,
    load_listing: Option<LoadListing>,
    stdout: Option<String>,
    stderr: Option<String>,
    success_codes: Vec<i64>,
    options: Options,
    /// The entries that the tool updates in place.
    in_place: Vec<InPlace>,
}

impl Pending {
    pub(crate) fn in_place_sources(&self) -> impl Iterator<Item = &str> {
        self.in_place
            .iter()
            .filter(|entry| !entry.directory)
            .map(|entry| entry.location.as_str())
    }

    /// The new locations of the files that the tool updated in place, by their old ones: the
    /// copies that the task changed, which later steps see instead, as cwltool updates the
    /// file itself.
    pub fn updated(&self, outcome: &Outcome) -> Vec<(String, String)> {
        let mut updates = Vec::new();
        for entry in &self.in_place {
            let made = (outcome.files.iter())
                .find(|file| file.path.trim_end_matches('/') == entry.path.trim_end_matches('/'));
            if let Some(file) = made {
                let location = if cwl_parser::iri::has_scheme(&file.location) {
                    file.location.clone()
                } else {
                    cwl_parser::iri::file_url(std::path::Path::new(&file.location))
                };
                updates.push((entry.location.clone(), location));
            }
        }
        updates
    }
}

/// Makes the task of a command line tool (or of an abstract `Operation`) for a prepared
/// input object. `fake` says that its files are placeholders or outputs of the simulator,
/// whose content is not real.
pub fn task(
    process: &Arc<Process>,
    job: Map,
    scope: &Scope,
    options: &Options,
    id: TaskId,
    names: (&str, &str),
    fake: bool,
) -> Result<(Task, Pending), String> {
    let (name, step) = names;
    let evaluator = scope.evaluator(&options.javascript);
    let mut types = scope.types();
    for input in &process.inputs {
        types.add(&input.value_type);
    }
    for output in &process.outputs {
        types.add(&output.value_type);
    }
    let tool = match &process.class {
        Class::CommandLineTool(tool) => Some(tool.as_ref()),
        _ => None,
    };
    let docker = scope.find(|class| match class {
        RequirementClass::Docker(docker) => Some(docker),
        _ => None,
    });
    let outdir = docker
        .and_then(|(docker, _)| docker.docker_output_directory.clone())
        .unwrap_or_else(|| OUTDIR.to_owned());
    let image = crate::image::select(docker, options)?;
    let job = Value::Map(job);
    let mut runtime = Map::new();
    runtime.insert("outdir".into(), Value::string(outdir.as_str()));
    runtime.insert("tmpdir".into(), Value::string(TMPDIR));
    let (resources, sizes) = resources(scope, &evaluator, &job, &runtime, process.version)
        .map_err(|error| format!("the ResourceRequirement: {error}"))?;
    for (key, value) in sizes {
        runtime.insert(key.into(), value);
    }

    // Stage input files, then InitialWorkDir entries, which win for the same location.
    let mut stager = Stager {
        outdir: outdir.clone(),
        ..Stager::default()
    };
    stager.stage_job(&job);
    let mut inputs = job;
    adjust(&mut inputs, &stager.paths);
    if tool.is_some() {
        initial_workdir(process, scope, &evaluator, &inputs, &runtime, &mut stager)?;
    }
    adjust(&mut inputs, &stager.paths);
    let runtime_value = Value::Map(runtime.clone());
    let null = Value::Null;
    let vars = Variables {
        inputs: &inputs,
        this: &null,
        runtime: &runtime_value,
    };

    // Standard streams.
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    if let Some(tool) = tool {
        if let Some(expression) = &tool.stdin {
            let value = evaluator
                .evaluate(expression, vars)
                .map_err(|error| format!("`stdin`: {error}"))?;
            match value {
                Value::String(path) if !path.is_empty() => stdin = Some(path),
                Value::Null | Value::String(_) => {}
                other => return Err(format!("`stdin` must be a path, not {}", other.to_json())),
            }
        }
        for input in &process.inputs {
            if input.value_type == Type::Stdin
                && let Some(path) = inputs
                    .get(short_name(&input.id))
                    .and_then(|file| file.text("path"))
            {
                stdin = Some(path.to_owned());
            }
        }
        stdout = stream(
            tool.stdout.as_ref(),
            process,
            Type::Stdout,
            &evaluator,
            vars,
        )?;
        stderr = stream(
            tool.stderr.as_ref(),
            process,
            Type::Stderr,
            &evaluator,
            vars,
        )?;
    }

    // The command line.
    let mut bindings: Vec<Bound> = Vec::new();
    if let Some(tool) = tool {
        for (index, word) in tool.base_command.iter().enumerate() {
            let key = vec![Key::Number(-1_000_000), Key::Number(index as i64)];
            bindings.push(Bound::text(key, word));
        }
        for (index, argument) in tool.arguments.iter().enumerate() {
            let index = Key::Number(index as i64);
            bindings.push(match argument {
                Argument::Text(expression) if has_code(expression) => Bound {
                    value_from: Some(expression.clone()),
                    value: Value::Null,
                    ..Bound::text(vec![Key::Number(0), index], "")
                },
                Argument::Text(expression) => {
                    Bound::text(vec![Key::Number(0), index], &literal(expression))
                }
                Argument::Binding(binding) => {
                    let position = match &binding.position {
                        None => 0,
                        Some(Dynamic::Value(position)) => *position,
                        Some(Dynamic::Expression(expression)) => {
                            let value = evaluator
                                .evaluate(expression, vars)
                                .map_err(|error| format!("the command line: {error}"))?;
                            match value {
                                Value::Null => 0,
                                value => value.as_i64().ok_or_else(|| {
                                    format!(
                                        "`position` must be an integer, not {}",
                                        value.to_json()
                                    )
                                })?,
                            }
                        }
                    };
                    Bound {
                        key: vec![Key::Number(position), index],
                        value: Value::Null,
                        prefix: binding.prefix.clone(),
                        separate: binding.separate,
                        item_separator: binding.item_separator.clone(),
                        value_from: binding.value_from.clone(),
                        shell_quote: binding.shell_quote,
                    }
                }
            });
        }
        let mut positions = |expression: &Expression, this: &Value| {
            evaluator.evaluate(expression, Variables { this, ..vars })
        };
        let mut binder = Binder {
            types: &types,
            positions: &mut positions,
        };
        for input in &process.inputs {
            let name = short_name(&input.id);
            let value = inputs.get(name).cloned().unwrap_or(Value::Null);
            if value.is_null() {
                continue;
            }
            let bound =
                binder.bind_input(&input.value_type, input.binding.as_ref(), &value, name)?;
            bindings.extend(bound);
        }
        binding::sort(&mut bindings);
    }
    let mut args = Vec::new();
    for bound in &bindings {
        let value = match &bound.value_from {
            Some(expression) => match evaluator.evaluate(
                expression,
                Variables {
                    this: &bound.value,
                    ..vars
                },
            ) {
                Ok(value) => value,
                // An argument made from the content of files whose content is not real, as in
                // a simulated run, cannot be made; it becomes a marker, so the task still exists.
                Err(error)
                    if fake
                        && !crate::javascript::helper::boundary(&error)
                        && reads_contents(expression) =>
                {
                    Value::string(FAKE_ARGUMENT)
                }
                Err(error) => return Err(format!("the command line: {error}")),
            },
            None => bound.value.clone(),
        };
        args.extend(binding::arguments(bound, &value)?);
    }
    let shell = scope.has(|class| matches!(class, RequirementClass::ShellCommand));
    let redirections = [
        stdout
            .as_ref()
            .map(|path| format!("> {}", binding::shell_quote(path))),
        stderr
            .as_ref()
            .map(|path| format!("2> {}", binding::shell_quote(path))),
        stdin
            .as_ref()
            .map(|path| format!("< {}", binding::shell_quote(path))),
    ];
    let redirections: Vec<String> = redirections.into_iter().flatten().collect();
    let (command, script) = if tool.is_none() {
        (Vec::new(), None)
    } else if shell || !redirections.is_empty() {
        let words: Vec<String> = args
            .iter()
            .map(|arg| {
                if arg.quote || !shell {
                    binding::shell_quote(&arg.text)
                } else {
                    arg.text.clone()
                }
            })
            .collect();
        let mut script = String::new();
        if !redirections.is_empty() {
            script.push_str(&format!("exec {}\n", redirections.join(" ")));
        }
        script.push_str(&words.join(" "));
        let command = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()];
        (command, Some(script))
    } else {
        let command: Vec<String> = args.into_iter().map(|arg| arg.text).collect();
        // `bash -c <script>` runs a script too.
        let script = match command.as_slice() {
            [shell, flag, script, ..]
                if flag == "-c" && matches!(files::basename_of(shell).as_str(), "sh" | "bash") =>
            {
                Some(script.clone())
            }
            _ => None,
        };
        (command, script)
    };

    // Environment.
    let mut environment = BTreeMap::new();
    environment.insert("HOME".to_owned(), outdir.clone());
    environment.insert("TMPDIR".to_owned(), TMPDIR.to_owned());
    if let Some((variables, _)) = scope.find(|class| match class {
        RequirementClass::EnvVar { variables } => Some(variables),
        _ => None,
    }) {
        for variable in variables {
            let value = evaluator
                .evaluate(&variable.env_value, vars)
                .map_err(|error| format!("the EnvVarRequirement: {error}"))?;
            let text = match value {
                Value::String(text) => text,
                other => other.to_python_text(),
            };
            environment.insert(variable.env_name.clone(), text);
        }
    }

    // Outputs the executor fetches: the standard streams, the globs, and cwl.output.json
    // when an output can come from nowhere else.
    let mut outputs: Vec<String> = stdout.iter().chain(&stderr).cloned().collect();
    // The copies that the tool updates in place come back, as the new versions of their files.
    for entry in &stager.in_place {
        outputs.push(match entry.directory {
            true => format!("{}/", entry.path),
            false => entry.path.clone(),
        });
    }
    let mut needs_json = false;
    for output in &process.outputs {
        match types.resolve(&output.value_type) {
            Type::Stdout | Type::Stderr => {}
            _ if tool.is_none() => outputs.extend(operation_output(&types, output)),
            _ => {
                needs_json |= unbound(&types, &output.value_type, output.binding.as_ref());
                patterns(
                    &types,
                    &output.value_type,
                    output.binding.as_ref(),
                    &output.secondary_files,
                    &evaluator,
                    vars,
                    &outdir,
                    &mut outputs,
                )
                .map_err(|error| format!("the output `{}`: {error}", short_name(&output.id)))?;
            }
        }
    }
    // The glob `.` names the output directory itself, and cwl.output.json, an `outputEval` that
    // gives files and secondary files that expressions name may name any file of it, so a real
    // run keeps all of it, as cwltool keeps the directory; the simulator makes no such files.
    let whole = process.outputs.iter().any(|output| {
        let named = (output.secondary_files.iter()).any(|entry| has_code(&entry.pattern));
        let Some(binding) = output.binding.as_ref() else {
            return false;
        };
        let found = outputs::globs(binding, &evaluator, vars, &outdir);
        let dot = found.is_ok_and(|globs| globs.iter().any(|glob| glob == "."));
        let evaluated =
            binding.output_eval.is_some() && outputs::holds_files(&types, &output.value_type);
        dot || evaluated || named
    });
    if needs_json {
        outputs.push(OUTPUT_JSON.to_owned());
    }
    if (needs_json || whole) && !options.simulated {
        outputs.push("*".to_owned());
    }
    let mut unique = Vec::new();
    for output in outputs {
        if !unique.contains(&output) {
            unique.push(output);
        }
    }
    let resources = Resources {
        gpus: scope.gpus(),
        ..resources
    };
    let time = scope.find(|class| match class {
        RequirementClass::ToolTimeLimit { timelimit } => Some(timelimit),
        _ => None,
    });
    let time_seconds = match time {
        Some((Dynamic::Value(seconds), _)) => Some(Value::Integer(*seconds)),
        Some((Dynamic::Expression(expression), _)) => Some(evaluator.evaluate(expression, vars)?),
        None => None,
    };
    // Zero means no limit; as in cwltool, a limit that is no whole number of 0 or more fails.
    let time_seconds = match time_seconds {
        None | Some(Value::Null) => None,
        Some(limit) => match limit.as_i64().map(u64::try_from) {
            Some(Ok(seconds)) => (seconds > 0).then_some(seconds),
            _ => {
                return Err(format!(
                    "`timelimit` must be a whole number of 0 or more, not {}",
                    limit.to_json()
                ));
            }
        },
    };
    // A tool has no network unless it asks for it; v1.0 tools ask for it by the upgrade.
    let network = scope.find(|class| match class {
        RequirementClass::NetworkAccess { network_access } => Some(network_access),
        _ => None,
    });
    let network = match network {
        Some((Dynamic::Value(allowed), _)) => *allowed,
        Some((Dynamic::Expression(expression), _)) => {
            match evaluator.evaluate(expression, vars)? {
                Value::Bool(allowed) => allowed,
                other => {
                    return Err(format!(
                        "`networkAccess` must give a boolean, not {}",
                        other.to_json()
                    ));
                }
            }
        }
        None => false,
    };
    // Reuse is on unless `WorkReuse` turns it off.
    let reuse = scope.find(|class| match class {
        RequirementClass::WorkReuse { enable_reuse } => Some(enable_reuse),
        _ => None,
    });
    let reuse = match reuse {
        Some((Dynamic::Value(enabled), _)) => *enabled,
        Some((Dynamic::Expression(expression), _)) => {
            match evaluator.evaluate(expression, vars)? {
                Value::Bool(enabled) => enabled,
                other => {
                    return Err(format!(
                        "`enableReuse` must give a boolean, not {}",
                        other.to_json()
                    ));
                }
            }
        }
        None => true,
    };
    let task = Task {
        run_as: None,
        id,
        name: name.to_owned(),
        step: step.to_owned(),
        image: tool.map(|_| image),
        command,
        script,
        workdir: Some(outdir.clone()),
        environment,
        inputs: stager.inputs,
        outputs: unique,
        stdin: None,
        resources: Resources {
            time_seconds,
            ..resources
        },
        network: match (tool.is_some(), network) {
            // An abstract operation runs no tool.
            (false, _) => Network::Unstated,
            (true, true) => Network::Allowed,
            (true, false) => Network::Denied,
        },
        after: Vec::new(),
        containers: Vec::new(),
        replaces_entrypoint: false,
        reuse: match reuse {
            true => Reuse::Allowed,
            false => Reuse::Denied,
        },
    };
    // Outputs at the path of an input file get its location back, as with cwltool's path
    // mapper, which holds the files of the input object but not literals or writable copies.
    let staged = stager
        .paths
        .iter()
        .filter(|(location, _)| {
            stager.from_job.contains(*location) && !stager.writable.contains(*location)
        })
        .map(|(location, path)| {
            (
                path.clone(),
                (location.clone(), stager.directories.contains(location)),
            )
        })
        .collect();
    let pending = Pending {
        process: Arc::clone(process),
        evaluator,
        types,
        inputs,
        runtime,
        outdir,
        staged,
        load_listing: scope.load_listing(),
        stdout,
        stderr,
        success_codes: tool
            .map(|tool| tool.success_codes.clone())
            .unwrap_or_default(),
        options: options.clone(),
        in_place: stager.in_place.clone(),
    };
    Ok((task, pending))
}

pub(crate) fn unsupported(message: &str) -> String {
    format!("{UNSUPPORTED}{message}")
}

/// Marks error messages of features iunctus does not support, so the driver reports them
/// with exit code 33.
pub const UNSUPPORTED: &str = "unsupported: ";

/// The text of an argument without code, with its escapes applied.
fn literal(expression: &Expression) -> String {
    expression
        .parts
        .iter()
        .map(|part| match part {
            cwl_parser::model::Part::Text(text) => text.as_str(),
            _ => "",
        })
        .collect()
}

/// The `runtime` fields that resources give, by name.
type Runtime = Vec<(&'static str, Value)>;

/// The resources of a task, and the `runtime` fields they give: `cores`, `ram`,
/// `outdirSize` and `tmpdirSize`, with cwltool's defaults.
fn resources(
    scope: &Scope,
    evaluator: &Evaluator,
    job: &Value,
    runtime: &Map,
    version: Version,
) -> Result<(Resources, Runtime), String> {
    let requirement = scope.find(|class| match class {
        RequirementClass::Resource(resource) => Some(resource.as_ref()),
        _ => None,
    });
    let runtime = Value::Map(runtime.clone());
    let vars = Variables {
        inputs: job,
        this: &Value::Null,
        runtime: &runtime,
    };
    let amount = |field: &Option<Dynamic<f64>>| -> Result<Option<f64>, String> {
        match field {
            None => Ok(None),
            Some(Dynamic::Value(amount)) => Ok(Some(*amount)),
            Some(Dynamic::Expression(expression)) => {
                let value = evaluator.evaluate(expression, vars)?;
                match value {
                    Value::Null => Ok(None),
                    value => value.as_f64().map(Some).ok_or_else(|| {
                        format!("a resource must be a number, not {}", value.to_json())
                    }),
                }
            }
        }
    };
    let ram_default = if version == Version::V1_0 {
        1024.0
    } else {
        256.0
    };
    let mut cores = 1.0;
    let mut ram = ram_default;
    let mut tmpdir = 1024.0;
    let mut outdir = 1024.0;
    if let Some((resource, _)) = requirement {
        let pick = |min: Option<f64>, max: Option<f64>, default: f64| {
            if min
                .into_iter()
                .chain(max)
                .any(|value| !value.is_finite() || value < 0.0)
                || matches!((min, max), (Some(min), Some(max)) if min > max)
            {
                return Err(
                    "resource amounts must be finite, nonnegative and min <= max".to_owned(),
                );
            }
            Ok(min.or(max).unwrap_or(default))
        };
        cores = pick(
            amount(&resource.cores_min)?,
            amount(&resource.cores_max)?,
            cores,
        )?;
        ram = pick(amount(&resource.ram_min)?, amount(&resource.ram_max)?, ram)?;
        tmpdir = pick(
            amount(&resource.tmpdir_min)?,
            amount(&resource.tmpdir_max)?,
            tmpdir,
        )?;
        outdir = pick(
            amount(&resource.outdir_min)?,
            amount(&resource.outdir_max)?,
            outdir,
        )?;
    }
    let mebibytes = |amount: f64| {
        (amount.ceil() as u64)
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "resource amount exceeds the supported byte range".to_owned())
    };
    let resources = Resources {
        cpus: Some(cores),
        memory_bytes: Some(mebibytes(ram)?),
        disk_bytes: Some(
            mebibytes(tmpdir)?
                .checked_add(mebibytes(outdir)?)
                .ok_or_else(|| {
                    "combined disk resource amount exceeds the supported byte range".to_owned()
                })?,
        ),
        time_seconds: None,
        gpus: None,
    };
    let sizes = vec![
        ("cores", Value::Integer(cores.ceil() as i64)),
        ("ram", Value::Integer(ram.ceil() as i64)),
        ("outdirSize", Value::Integer(outdir.ceil() as i64)),
        ("tmpdirSize", Value::Integer(tmpdir.ceil() as i64)),
    ];
    Ok((resources, sizes))
}

/// The file name of `stdout` or `stderr`: evaluated, or a name made from the tool when an
/// output of that type needs one.
fn stream(
    field: Option<&Expression>,
    process: &Process,
    kind: Type,
    evaluator: &Evaluator,
    vars: Variables<'_>,
) -> Result<Option<String>, String> {
    let label = if kind == Type::Stdout {
        "stdout"
    } else {
        "stderr"
    };
    if let Some(expression) = field {
        let value = evaluator
            .evaluate(expression, vars)
            .map_err(|error| format!("`{label}`: {error}"))?;
        let Value::String(path) = value else {
            return Err(format!(
                "`{label}` must be a file name, not {}",
                value.to_json()
            ));
        };
        if path.starts_with('/') || path.split('/').any(|part| part == "..") {
            return Err(format!("`{label}` must be a relative path, not `{path}`"));
        }
        return Ok((!path.is_empty()).then_some(path));
    }
    let needed = process
        .outputs
        .iter()
        .any(|output| output.value_type == kind);
    Ok(needed.then(|| {
        let digest = files::checksum_of(format!("{}#{label}", process.id).as_bytes());
        format!("{}.{label}", &digest[5..21])
    }))
}

/// The argument in place of one made from the content of files that are not real.
pub const FAKE_ARGUMENT: &str = "<made from the content of simulated files>";

/// Whether an expression reads the `contents` of files.
fn reads_contents(expression: &Expression) -> bool {
    expression.parts.iter().any(|part| match part {
        cwl_parser::model::Part::Text(_) => false,
        cwl_parser::model::Part::Reference(code)
        | cwl_parser::model::Part::Javascript(code)
        | cwl_parser::model::Part::Function(code) => code.contains("contents"),
    })
}

#[cfg(test)]
mod tests;
