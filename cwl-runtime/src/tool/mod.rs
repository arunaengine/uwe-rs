//! Command line tools: a prepared input object becomes a task, with staged files, the
//! command line, environment, image and resources; the outcome of the task becomes the
//! output object. This follows cwltool's `CommandLineTool.job` and `collect_output`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use cwl_parser::iri;
use cwl_parser::model::{
    Argument, Class, Dynamic, Expression, LoadListing, Process, RequirementClass, Type, Version,
    short_name,
};
use workflow_graph::{Outcome, Resources, Task, TaskId};

use crate::binding::{self, Binder, Bound, Key};
use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
use crate::inputs::has_code;
use crate::scope::Scope;
use crate::types::Types;
use crate::value::{Map, Value};

mod collect;
mod outputs;
mod stage;
mod workdir;

pub use collect::workdir;
use outputs::{operation_output, patterns, unbound};
use stage::{Stager, adjust};
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Take absent files as present and empty, and give outputs that the executor cannot
    /// make (like the content of `cwl.output.json`) placeholder values of their type. This
    /// is what the mock executor needs, and the default while it is the only executor.
    pub mock: bool,
    /// The image of tools without `DockerRequirement`.
    pub image: String,
    /// On placeholder inputs, give the optional inputs of the process the run starts with
    /// placeholders too, and its flags `true`, as a job that sets every input and turns
    /// everything on; by default optional inputs are null and flags `false`.
    pub fill_optional: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mock: true,
            image: DEFAULT_IMAGE.into(),
            fill_optional: false,
        }
    }
}

impl Options {
    pub fn access(&self) -> Access {
        Access {
            assume_present: self.mock,
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
    staged: HashMap<String, String>,
    load_listing: Option<LoadListing>,
    stdout: Option<String>,
    stderr: Option<String>,
    success_codes: Vec<i64>,
    options: Options,
}

/// Makes the task of a command line tool (or of an abstract `Operation`) for a prepared
/// input object. `fake` says that its files are placeholders or outputs of the mock executor,
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
    let evaluator = scope.evaluator();
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
    let image = match docker {
        Some((docker, required)) => match docker
            .docker_pull
            .as_ref()
            .or(docker.docker_image_id.as_ref())
        {
            Some(image) => image.clone(),
            None if required => {
                return Err(unsupported(
                    "DockerRequirement with dockerFile, dockerLoad or dockerImport needs an \
                     image build, which iunctus does not do yet; give dockerPull",
                ));
            }
            None => options.image.clone(),
        },
        None => options.image.clone(),
    };
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
                // a mock run, cannot be made; it becomes a marker, so the task still exists.
                Err(_) if fake && reads_contents(expression) => Value::string(FAKE_ARGUMENT),
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
    if needs_json {
        outputs.push(OUTPUT_JSON.to_owned());
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
    let task = Task {
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
        after: Vec::new(),
    };
    // Outputs at the path of an input file get its location back, as with cwltool's path
    // mapper, which holds the files of the input object but not literals or writable copies.
    let staged = stager
        .paths
        .iter()
        .filter(|(location, _)| {
            stager.from_job.contains(*location) && !stager.writable.contains(*location)
        })
        .map(|(location, path)| (path.clone(), location.clone()))
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
    };
    Ok((task, pending))
}

fn unsupported(message: &str) -> String {
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
        let pick =
            |min: Option<f64>, max: Option<f64>, default: f64| min.or(max).unwrap_or(default);
        cores = pick(
            amount(&resource.cores_min)?,
            amount(&resource.cores_max)?,
            cores,
        );
        ram = pick(amount(&resource.ram_min)?, amount(&resource.ram_max)?, ram);
        tmpdir = pick(
            amount(&resource.tmpdir_min)?,
            amount(&resource.tmpdir_max)?,
            tmpdir,
        );
        outdir = pick(
            amount(&resource.outdir_min)?,
            amount(&resource.outdir_max)?,
            outdir,
        );
    }
    let mebibytes = |amount: f64| (amount.ceil().max(0.0) as u64) * 1024 * 1024;
    let resources = Resources {
        cpus: Some(cores),
        memory_bytes: Some(mebibytes(ram)),
        disk_bytes: Some(mebibytes(tmpdir) + mebibytes(outdir)),
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

/// Collects outputs from the files of an outcome.
struct Collector<'a> {
    pending: &'a Pending,
    outcome: &'a Outcome,
    runtime: Value,
    access: Access,
    workdir: Option<String>,
}

impl Collector<'_> {
    fn vars<'v>(&'v self, this: &'v Value) -> Variables<'v> {
        Variables {
            inputs: &self.pending.inputs,
            this,
            runtime: &self.runtime,
        }
    }

    fn evaluate(&self, expression: &Expression, this: &Value) -> Result<Value, String> {
        self.pending.evaluator.evaluate(expression, self.vars(this))
    }

    /// Gives every file object of a value its host location, as cwltool's `revmap_file`.
    fn revmap_all(&self, value: &mut Value) -> Result<(), String> {
        match value {
            Value::List(items) => {
                for item in items {
                    self.revmap_all(item)?;
                }
                Ok(())
            }
            Value::Map(_) if value.is_file_like() => {
                self.revmap(value)?;
                if let Value::Map(map) = value {
                    for key in ["secondaryFiles", "listing"] {
                        if let Some(inner) = map.get_mut(key) {
                            self.revmap_all(inner)?;
                        }
                    }
                }
                Ok(())
            }
            Value::Map(map) => {
                for (_, item) in map.iter_mut() {
                    self.revmap_all(item)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn revmap(&self, value: &mut Value) -> Result<(), String> {
        let Value::Map(map) = value else {
            return Ok(());
        };
        let location = map
            .get("location")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut path = map.get("path").and_then(Value::as_str).map(str::to_owned);
        // A `file:` location inside the output directory of the container, as
        // `cwl.output.json` may give one, names a path there.
        let outdir = &self.pending.outdir;
        if path.is_none()
            && let Some(file_path) = location.as_deref().and_then(iri::file_path)
            && file_path.starts_with(outdir)
        {
            path = Some(file_path.to_string_lossy().into_owned());
        }
        match (location, path) {
            (Some(location), None) => {
                if !iri::has_scheme(&location) && !location.starts_with("_:") {
                    let base = self.workdir.clone().unwrap_or_default();
                    let joined = iri::file_url(&Path::new(&base).join(&location));
                    map.insert("location".into(), Value::String(joined));
                }
                Ok(())
            }
            (_, Some(path)) => {
                map.shift_remove("dirname");
                let outdir = &self.pending.outdir;
                let absolute = if path.starts_with('/') {
                    path.clone()
                } else {
                    format!("{outdir}/{path}")
                };
                map.shift_remove("path");
                if !map.contains_key("basename") {
                    map.insert(
                        "basename".into(),
                        Value::String(files::basename_of(&absolute)),
                    );
                }
                let location = if let Some(original) = self.pending.staged.get(&absolute) {
                    original.clone()
                } else if absolute == *outdir || absolute.starts_with(&format!("{outdir}/")) {
                    let relative = absolute[outdir.len()..].trim_start_matches('/');
                    let base = self.workdir.clone().unwrap_or_default();
                    iri::file_url(&Path::new(&base).join(relative))
                } else if absolute.starts_with(TMPDIR) || absolute.starts_with(STAGING) {
                    return Err(format!(
                        "the output `{path}` is outside the output directory"
                    ));
                } else {
                    // A path from the host, such as a file in a mock work folder.
                    iri::file_url(Path::new(&absolute))
                };
                map.insert("location".into(), Value::String(location));
                // Keep the container path for expressions that come after, like secondaryFiles.
                map.insert("path".into(), Value::String(absolute));
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    /// Adds size and checksum to the files of a value that lack them.
    fn checksums(&self, value: &mut Value) -> Result<(), String> {
        let access = self.access;
        let mut error = None;
        files::visit_files(value, &mut |file| {
            let is_file = file.get("class").and_then(Value::as_str) == Some("File");
            if is_file
                && !file.contains_key("checksum")
                && let Err(message) = access.add_checksum(file)
            {
                error.get_or_insert(message);
            }
        });
        match error {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }
}

/// The argument in place of one made from the content of files that are not real.
pub const FAKE_ARGUMENT: &str = "<made from the content of mock files>";

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
