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
    Argument, Class, Dynamic, Expression, LoadListing, OutputBinding, OutputParameter, Process,
    RequirementClass, SecondaryFile, Type, Version, short_name,
};
use workflow_graph::mock::glob_matches;
use workflow_graph::{Outcome, OutputFile, Resources, Task, TaskId};

use crate::binding::{self, Binder, Bound, Key};
use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
use crate::inputs::has_code;
use crate::placeholder;
use crate::scope::Scope;
use crate::types::Types;
use crate::value::{Map, Value};

mod stage;
mod workdir;

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

/// The patterns an output asks the executor for: its globs, with `/` for directories, and
/// the names of its secondary files.
#[allow(clippy::too_many_arguments)]
fn patterns(
    types: &Types,
    value_type: &Type,
    binding: Option<&OutputBinding>,
    secondary: &[SecondaryFile],
    evaluator: &Evaluator,
    vars: Variables<'_>,
    outdir: &str,
    patterns: &mut Vec<String>,
) -> Result<(), String> {
    if let Some(binding) = binding {
        let directory = directory_type(types, value_type);
        for glob in globs(binding, evaluator, vars, outdir)? {
            if glob == "." {
                continue;
            }
            for entry in secondary {
                if !has_code(&entry.pattern) {
                    patterns.push(files::substitute(&glob, &entry.pattern.text));
                }
            }
            patterns.push(if directory { format!("{glob}/") } else { glob });
        }
    }
    if let Type::Record(record) = types.resolve(value_type)
        && binding.is_none()
    {
        for field in &record.fields {
            self::patterns(
                types,
                &field.value_type,
                field.output_binding.as_ref(),
                &field.secondary_files,
                evaluator,
                vars,
                outdir,
                patterns,
            )?;
        }
    }
    Ok(())
}

/// Whether an output, or a field of its record, has no binding, so that only
/// `cwl.output.json` can give it.
fn unbound(types: &Types, value_type: &Type, binding: Option<&OutputBinding>) -> bool {
    if binding.is_some() {
        return false;
    }
    match types.resolve(value_type) {
        Type::Record(record) => record
            .fields
            .iter()
            .any(|field| unbound(types, &field.value_type, field.output_binding.as_ref())),
        Type::Stdout | Type::Stderr => false,
        _ => true,
    }
}

/// Whether a type takes files and whether it takes directories, in lists and unions too.
fn classes(types: &Types, value_type: &Type) -> (bool, bool) {
    match types.resolve(value_type) {
        Type::File | Type::Stdout | Type::Stderr => (true, false),
        Type::Directory => (false, true),
        Type::Any => (true, true),
        Type::Array(array) => classes(types, &array.items),
        Type::Union(members) => members
            .iter()
            .fold((false, false), |(files, folders), member| {
                let (file, folder) = classes(types, member);
                (files || file, folders || folder)
            }),
        _ => (false, false),
    }
}

/// Whether the files an output collects are directories.
fn directory_type(types: &Types, value_type: &Type) -> bool {
    match types.resolve(value_type) {
        Type::Directory => true,
        Type::Array(array) => directory_type(types, &array.items),
        Type::Union(members) => {
            let others: Vec<&Type> = members
                .iter()
                .filter(|member| !matches!(member, Type::Null))
                .collect();
            !others.is_empty() && others.iter().all(|member| directory_type(types, member))
        }
        _ => false,
    }
}

/// The glob patterns of an output binding, relative to the output directory; `.` stands
/// for the output directory itself.
fn globs(
    binding: &OutputBinding,
    evaluator: &Evaluator,
    vars: Variables<'_>,
    outdir: &str,
) -> Result<Vec<String>, String> {
    let mut patterns = Vec::new();
    for glob in &binding.glob {
        let value = evaluator.evaluate(glob, vars)?;
        let values = match value {
            Value::Null => Vec::new(),
            Value::String(text) => vec![text],
            Value::List(items) => {
                let mut texts = Vec::new();
                for item in items {
                    match item {
                        Value::String(text) => texts.push(text),
                        other => {
                            return Err(format!(
                                "a glob must be a string, not {}",
                                other.summary()
                            ));
                        }
                    }
                }
                texts
            }
            other => return Err(format!("a glob must be a string, not {}", other.summary())),
        };
        for text in values {
            if text.is_empty() {
                continue;
            }
            let relative = if text == outdir {
                ".".to_owned()
            } else if let Some(rest) = text.strip_prefix(&format!("{outdir}/")) {
                rest.to_owned()
            } else if text.starts_with('/') {
                return Err(format!("the glob `{text}` must not start with `/`"));
            } else {
                text
            };
            let relative = relative
                .strip_prefix("./")
                .map(str::to_owned)
                .unwrap_or(relative);
            patterns.push(if relative.is_empty() {
                ".".to_owned()
            } else {
                relative
            });
        }
    }
    Ok(patterns)
}

/// The file an abstract `Operation` is taken to make for an output, named after it.
fn operation_output(types: &Types, output: &OutputParameter) -> Option<String> {
    let name = short_name(&output.id);
    let mut value_type = types.resolve(&output.value_type);
    loop {
        match value_type {
            Type::Array(array) => value_type = types.resolve(&array.items),
            Type::Union(members) => {
                let member = members
                    .iter()
                    .find(|member| !matches!(member, Type::Null))?;
                value_type = types.resolve(member);
            }
            Type::File | Type::Stdout | Type::Stderr => return Some(name.to_owned()),
            Type::Directory => return Some(format!("{name}/")),
            _ => return None,
        }
    }
}

impl Pending {
    /// Whether the task is an abstract `Operation`, which runs no tool.
    pub fn is_abstract(&self) -> bool {
        matches!(self.process.class, Class::Operation)
    }

    /// The output object of the task from its outcome.
    pub fn collect(self, outcome: &Outcome) -> Result<Map, String> {
        let exit = i64::from(outcome.exit_code);
        if exit != 0 && !self.success_codes.contains(&exit) {
            return Err(format!("the command ended with exit code {exit}"));
        }
        let mut runtime = self.runtime.clone();
        runtime.insert("exitCode".into(), Value::Integer(exit));
        let collector = Collector {
            pending: &self,
            outcome,
            runtime: Value::Map(runtime),
            access: self.options.access(),
            workdir: workdir(outcome),
        };
        let from_json = match outcome.files.iter().find(|file| file.path == OUTPUT_JSON) {
            Some(file) => {
                let text = std::fs::read_to_string(&file.location).unwrap_or_default();
                match Value::parse(&text) {
                    Ok(Value::Map(map)) => Some(map),
                    // The mock makes an empty file, which gives no outputs.
                    _ if self.options.mock => None,
                    Ok(_) => return Err(format!("{OUTPUT_JSON} must hold an object")),
                    Err(error) => return Err(format!("{OUTPUT_JSON}: {error}")),
                }
            }
            None => None,
        };
        let is_operation = !matches!(self.process.class, Class::CommandLineTool(_));
        let mut outputs = Map::new();
        if let Some(mut map) = from_json {
            for (_, value) in map.iter_mut() {
                collector.revmap_all(value)?;
            }
            let mut value = Value::Map(map);
            files::normalize(&mut value)?;
            let Value::Map(map) = value else {
                unreachable!("an object stays a map")
            };
            outputs = map;
            for (_, value) in outputs.iter_mut() {
                collector.checksums(value)?;
            }
        } else {
            for output in &self.process.outputs {
                let name = short_name(&output.id).to_owned();
                let collected = if is_operation {
                    collector.operation(output)
                } else if output.binding.is_none()
                    && !matches!(
                        self.types.resolve(&output.value_type),
                        Type::Stdout | Type::Stderr | Type::Record(_)
                    )
                {
                    // Only cwl.output.json gives this output; a mock run has none.
                    Ok(Value::Null)
                } else {
                    collector.output(
                        &name,
                        &output.value_type,
                        output.binding.as_ref(),
                        &output.secondary_files,
                        output.format.as_ref(),
                    )
                };
                let value = collected.map_err(|error| format!("the output `{name}`: {error}"))?;
                outputs.insert(name, value);
            }
        }
        for output in &self.process.outputs {
            let name = short_name(&output.id).to_owned();
            let value = outputs.get(&name).cloned().unwrap_or(Value::Null);
            if !self.types.matches(&output.value_type, &value) {
                if self.options.mock {
                    let placeholder = self.placeholder(output, &collector);
                    outputs.insert(name, placeholder);
                } else {
                    return Err(format!(
                        "the output `{name}` is of type {}, but its value is {}",
                        crate::inputs::describe(&output.value_type),
                        value.summary()
                    ));
                }
            }
        }
        for (_, value) in outputs.iter_mut() {
            files::visit_files(value, &mut |file| {
                file.shift_remove("path");
                file.shift_remove("dirname");
            });
        }
        Ok(outputs)
    }

    /// A value for an output the executor could not make, with files in its work folder.
    fn placeholder(&self, output: &OutputParameter, collector: &Collector<'_>) -> Value {
        let base = match &collector.workdir {
            Some(folder) => format!("{}/", iri::file_url(Path::new(folder))),
            None => "_:".to_owned(),
        };
        let name = short_name(&output.id);
        let hint = placeholder::hint_of(name, output.format.as_slice(), &output.secondary_files);
        let mut value = placeholder::value(&self.types, &output.value_type, name, &base, &hint);
        // Placeholder files point at the task's JSON output, so the graph keeps the link.
        if let Some(json) = collector
            .outcome
            .files
            .iter()
            .find(|file| file.path == OUTPUT_JSON)
        {
            let location = iri::file_url(&json.location);
            files::visit_files(&mut value, &mut |file| {
                if file.get("class").and_then(Value::as_str) == Some("File") {
                    let name = file
                        .get("location")
                        .and_then(Value::as_str)
                        .map(files::basename_of)
                        .unwrap_or_default();
                    file.insert("basename".into(), Value::String(name));
                    file.insert("location".into(), Value::String(location.clone()));
                }
            });
        }
        let _ = files::normalize(&mut value);
        let access = self.options.access();
        files::visit_files(&mut value, &mut |file| {
            if file.get("class").and_then(Value::as_str) == Some("File") {
                let _ = access.add_checksum(file);
            } else {
                access.load_listing(file, false);
            }
        });
        // The secondary files the output declares come with it, as with a real run.
        if !output.secondary_files.is_empty() {
            let _ = collector.secondary_files(&mut value, &output.secondary_files);
        }
        value
    }
}

/// The folder the executor put the task's outputs in, from any output file.
pub fn workdir(outcome: &Outcome) -> Option<String> {
    outcome.files.iter().find_map(|file| {
        let location = file.location.to_string_lossy();
        location
            .strip_suffix(file.path.trim_end_matches('/'))
            .map(|folder| folder.trim_end_matches('/').to_owned())
    })
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

    /// cwltool's `collect_output` for one output or record field.
    fn output(
        &self,
        name: &str,
        value_type: &Type,
        binding: Option<&OutputBinding>,
        secondary: &[SecondaryFile],
        format: Option<&Expression>,
    ) -> Result<Value, String> {
        let types = &self.pending.types;
        let resolved = types.resolve(value_type);
        let binding = match (binding, resolved) {
            (Some(binding), _) => Some(binding.clone()),
            (None, Type::Stdout | Type::Stderr) => {
                let name = if matches!(resolved, Type::Stdout) {
                    &self.pending.stdout
                } else {
                    &self.pending.stderr
                };
                name.as_ref().map(|name| OutputBinding {
                    glob: vec![Expression {
                        text: name.clone(),
                        parts: vec![cwl_parser::model::Part::Text(name.clone())],
                    }],
                    load_contents: None,
                    load_listing: None,
                    output_eval: None,
                })
            }
            _ => None,
        };
        let Some(binding) = binding else {
            if let Type::Record(record) = resolved {
                let mut map = Map::new();
                for field in &record.fields {
                    let value = self.output(
                        &field.name,
                        &field.value_type,
                        field.output_binding.as_ref(),
                        &field.secondary_files,
                        field.format.first(),
                    )?;
                    map.insert(field.name.clone(), value);
                }
                return Ok(Value::Map(map));
            }
            return Ok(Value::Null);
        };
        let null = Value::Null;
        let patterns = globs(
            &binding,
            &self.pending.evaluator,
            self.vars(&null),
            &self.pending.outdir,
        )?;
        // Without `outputEval` the matches are the value, so they must be of a class the
        // output takes, as cwltool's type check of outputs finds.
        let (files, directories) = classes(types, value_type);
        let checked = binding.output_eval.is_none() && files != directories;
        let mut found: Vec<Value> = Vec::new();
        for pattern in &patterns {
            for file in self.matches(pattern) {
                if checked && file.directory != directories {
                    let (class, taken) = if file.directory {
                        ("directory", "files")
                    } else {
                        ("file", "directories")
                    };
                    return Err(format!(
                        "the glob `{pattern}` matches the {class} `{}`, but the output takes {taken}",
                        file.path
                    ));
                }
                self.check_link(&file)?;
                let mut object = self.file_object(&file, &binding)?;
                self.mock_extension(&mut object, &file, pattern, name, format, secondary);
                found.push(object);
            }
        }
        let (optional, single) = match resolved {
            Type::Union(members) => (
                members.contains(&Type::Null),
                members
                    .iter()
                    .any(|member| matches!(member, Type::File | Type::Directory)),
            ),
            Type::File | Type::Directory | Type::Stdout | Type::Stderr => (false, true),
            _ => (false, false),
        };
        let mut result = match &binding.output_eval {
            Some(expression) => match self.evaluate(expression, &Value::List(found)) {
                Ok(value) => value,
                // Expressions that read the files of a mock run read empty files and may
                // fail; the output then gets a placeholder, as when it does not fit its type.
                Err(_) if self.access.assume_present && reads_self(expression) => Value::Null,
                Err(error) => return Err(error),
            },
            None => Value::List(found),
        };
        if single {
            match result {
                Value::List(items) if items.is_empty() => {
                    if !optional && !self.access.assume_present {
                        return Err(format!("no output file matches {patterns:?}"));
                    }
                    result = Value::Null;
                }
                Value::List(mut items) if items.len() == 1 => result = items.remove(0),
                Value::List(items) if !self.access.assume_present => {
                    return Err(format!(
                        "{} files match {patterns:?}, where one is expected",
                        items.len()
                    ));
                }
                Value::List(mut items) => result = items.remove(0),
                other => result = other,
            }
        }
        if !secondary.is_empty() {
            self.secondary_files(&mut result, secondary)?;
        }
        if let Some(format) = format {
            let mut primaries: Vec<&mut Value> = match &mut result {
                Value::List(items) => items.iter_mut().collect(),
                other => vec![other],
            };
            for primary in primaries.iter_mut() {
                if !primary.is_file() {
                    continue;
                }
                let value = match self.evaluate(format, primary) {
                    Ok(value) => value,
                    // The format is metadata; over the inputs of a mock run it may not exist.
                    Err(_) if self.access.assume_present => Value::Null,
                    Err(error) => return Err(error),
                };
                if let (Value::Map(map), Value::String(format)) = (&mut **primary, value) {
                    map.insert("format".into(), Value::String(format));
                }
            }
        }
        self.revmap_all(&mut result)?;
        // cwltool keeps 0 and "" but turns other empty results of optional outputs into null.
        let kept = matches!(
            result,
            Value::Integer(0) | Value::Bool(false) | Value::String(_)
        ) || matches!(result, Value::Float(number) if number == 0.0);
        if optional && !result.is_truthy() && !kept {
            return Ok(Value::Null);
        }
        Ok(result)
    }

    /// The outcome files a glob pattern matches, sorted, with `.` for the work folder.
    fn matches(&self, pattern: &str) -> Vec<OutputFile> {
        if pattern == "." {
            return match &self.workdir {
                Some(folder) => vec![OutputFile {
                    path: ".".into(),
                    location: folder.into(),
                    directory: true,
                }],
                None => Vec::new(),
            };
        }
        let mut found: Vec<OutputFile> = Vec::new();
        for file in &self.outcome.files {
            let path = file.path.trim_end_matches('/');
            if glob_matches(pattern, path) {
                found.push(OutputFile {
                    path: path.to_owned(),
                    location: file.location.clone(),
                    directory: file.directory || file.location.is_dir(),
                });
            } else if file.directory || file.location.is_dir() {
                // A folder the executor gave back may hold what the pattern names.
                for (relative, location, directory) in walk(&file.location, path) {
                    if glob_matches(pattern, &relative) {
                        found.push(OutputFile {
                            path: relative,
                            location,
                            directory,
                        });
                    }
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found.dedup_by(|a, b| a.path == b.path);
        found
    }

    /// In a mock run, the file that the mock names after a bare wildcard, like `mock` for `*`,
    /// has no extension; its name takes the one the output suggests, as the tool's file has.
    fn mock_extension(
        &self,
        object: &mut Value,
        file: &OutputFile,
        pattern: &str,
        name: &str,
        format: Option<&Expression>,
        secondary: &[SecondaryFile],
    ) {
        let generic = workflow_graph::mock::file_name(pattern);
        let bare = generic != pattern && files::split_extension(&generic).1.is_empty();
        if !self.access.assume_present || !bare || file.path != generic {
            return;
        }
        let (Value::Map(map), Some(extension)) = (
            object,
            placeholder::suggested_extension(name, format, secondary),
        ) else {
            return;
        };
        map.insert(
            "basename".into(),
            Value::String(format!("{generic}{extension}")),
        );
        map.insert("nameroot".into(), Value::String(generic));
        map.insert("nameext".into(), Value::string(extension));
    }

    /// Fails for a matched file that is a symbolic link to a place outside the output
    /// directory, which the task cannot give back.
    fn check_link(&self, file: &OutputFile) -> Result<(), String> {
        let Ok(target) = std::fs::read_link(&file.location) else {
            return Ok(());
        };
        let outdir = Path::new(&self.pending.outdir);
        let inside = if target.is_absolute() {
            target.starts_with(outdir)
                || self
                    .workdir
                    .as_ref()
                    .is_some_and(|folder| target.starts_with(folder))
        } else {
            // A relative target may climb at most to the output directory.
            let mut depth = Path::new(&file.path).components().count() as isize - 1;
            target.components().all(|part| {
                match part {
                    std::path::Component::ParentDir => depth -= 1,
                    std::path::Component::Normal(_) => depth += 1,
                    _ => {}
                }
                depth >= 0
            })
        };
        if inside {
            return Ok(());
        }
        Err(format!(
            "the output `{}` links to `{}`, outside the output directory",
            file.path,
            target.display()
        ))
    }

    /// The file object of a matched file, with contents, listing and checksum as asked.
    fn file_object(&self, file: &OutputFile, binding: &OutputBinding) -> Result<Value, String> {
        let class = if file.directory { "Directory" } else { "File" };
        let name = Path::new(&file.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| files::basename_of(&file.location.to_string_lossy()));
        let mut map = Map::new();
        map.insert("class".into(), Value::string(class));
        map.insert(
            "location".into(),
            Value::String(iri::file_url(&file.location)),
        );
        let path = if file.path == "." {
            self.pending.outdir.clone()
        } else {
            format!("{}/{}", self.pending.outdir, file.path)
        };
        map.insert("path".into(), Value::String(path));
        map.insert("basename".into(), Value::String(name.clone()));
        if file.directory {
            let listing = binding.load_listing.or(self.pending.load_listing);
            if let Some(listing) = listing
                && listing != LoadListing::NoListing
            {
                self.access
                    .load_listing(&mut map, listing == LoadListing::DeepListing);
                let mut listing_value = map
                    .shift_remove("listing")
                    .unwrap_or(Value::List(Vec::new()));
                self.checksums(&mut listing_value)?;
                map.insert("listing".into(), listing_value);
            }
        } else {
            let (root, extension) = files::split_extension(&name);
            map.insert("nameroot".into(), Value::string(root));
            map.insert("nameext".into(), Value::string(extension));
            if binding.load_contents == Some(true) {
                self.access.load_contents(&mut map)?;
            }
            self.access.add_checksum(&mut map)?;
        }
        Ok(Value::Map(map))
    }

    /// Adds the secondary files of output files that exist.
    fn secondary_files(
        &self,
        result: &mut Value,
        secondary: &[SecondaryFile],
    ) -> Result<(), String> {
        let mut primaries: Vec<&mut Value> = match result {
            Value::List(items) => items.iter_mut().collect(),
            other => vec![other],
        };
        for primary in primaries.iter_mut() {
            let Value::Map(map) = &mut **primary else {
                continue;
            };
            if !map.contains_key("secondaryFiles") {
                map.insert("secondaryFiles".into(), Value::List(Vec::new()));
            }
            let path = map
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let prefix = match path.rfind('/') {
                Some(slash) => path[..=slash].to_owned(),
                None => String::new(),
            };
            for entry in secondary {
                let this = Value::Map(map.clone());
                let required = match &entry.required {
                    None => false,
                    Some(Dynamic::Value(required)) => *required,
                    Some(Dynamic::Expression(expression)) => {
                        self.evaluate(expression, &this)?.is_truthy()
                    }
                };
                let names = if has_code(&entry.pattern) {
                    self.evaluate(&entry.pattern, &this)?
                } else {
                    let basename = map
                        .get("basename")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Value::String(files::substitute(basename, &entry.pattern.text))
                };
                let names = match names {
                    Value::List(items) => items,
                    Value::Null => Vec::new(),
                    other => vec![other],
                };
                for name in names {
                    let item = match name {
                        Value::String(name) if name.is_empty() => continue,
                        Value::String(name) => {
                            let mut item = Map::new();
                            item.insert("path".into(), Value::String(format!("{prefix}{name}")));
                            item
                        }
                        Value::Map(item) => item,
                        _ => continue,
                    };
                    let mut item = Value::Map(item);
                    self.revmap(&mut item)?;
                    let location = item.text("location").unwrap_or_default().to_owned();
                    let on_disk = files::local_path(&location);
                    let exists = on_disk.as_ref().is_some_and(|path| path.exists());
                    let is_directory = on_disk.as_ref().is_some_and(|path| path.is_dir());
                    if !exists && !self.access.assume_present {
                        if required {
                            return Err(format!(
                                "the required secondary file `{location}` is missing"
                            ));
                        }
                        continue;
                    }
                    if let Value::Map(item_map) = &mut item {
                        item_map.shift_remove("path");
                        let class = if is_directory { "Directory" } else { "File" };
                        item_map.insert("class".into(), Value::string(class));
                        if !is_directory {
                            let _ = self.access.add_checksum(item_map);
                        }
                    }
                    files::normalize(&mut item)?;
                    if let Some(Value::List(list)) = map.get_mut("secondaryFiles") {
                        list.push(item);
                    }
                }
            }
        }
        Ok(())
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

    /// The value of an output of an `Operation`: the file named after it, or a placeholder.
    fn operation(&self, output: &OutputParameter) -> Result<Value, String> {
        let name = short_name(&output.id);
        let types = &self.pending.types;
        let found = self.matches(name);
        let binding = OutputBinding {
            glob: Vec::new(),
            load_contents: None,
            load_listing: None,
            output_eval: None,
        };
        let mut file = match found.first() {
            Some(file) => self.file_object(file, &binding)?,
            None => return Ok(self.pending.placeholder(output, self)),
        };
        if !output.secondary_files.is_empty() {
            self.secondary_files(&mut file, &output.secondary_files)?;
        }
        let mut value_type = types.resolve(&output.value_type);
        let mut wrap = 0;
        loop {
            match value_type {
                Type::Array(array) => {
                    wrap += 1;
                    value_type = types.resolve(&array.items);
                }
                Type::Union(members) => {
                    match members.iter().find(|member| !matches!(member, Type::Null)) {
                        Some(member) => value_type = types.resolve(member),
                        None => break,
                    }
                }
                _ => break,
            }
        }
        let mut value = file;
        for _ in 0..wrap {
            value = Value::List(vec![value]);
        }
        let mut value = value;
        files::visit_files(&mut value, &mut |file| {
            file.shift_remove("path");
        });
        Ok(value)
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

/// Whether an expression reads `self`, the files an output binding found.
fn reads_self(expression: &Expression) -> bool {
    expression.parts.iter().any(|part| match part {
        cwl_parser::model::Part::Text(_) => false,
        cwl_parser::model::Part::Reference(code)
        | cwl_parser::model::Part::Javascript(code)
        | cwl_parser::model::Part::Function(code) => code.contains("self"),
    })
}

/// The files and folders below a folder, relative to `prefix`.
fn walk(folder: &Path, prefix: &str) -> Vec<(String, std::path::PathBuf, bool)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = if prefix.is_empty() || prefix == "." {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let directory = path.is_dir();
        if directory {
            found.extend(walk(&path, &relative));
        }
        found.push((relative, path, directory));
    }
    found
}

#[cfg(test)]
mod tests;
