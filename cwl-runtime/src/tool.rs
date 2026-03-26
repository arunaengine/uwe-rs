//! Command line tools: a prepared input object becomes a task, with staged files, the
//! command line, environment, image and resources; the outcome of the task becomes the
//! output object. This follows cwltool's `CommandLineTool.job` and `collect_output`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use cwl_parser::iri;
use cwl_parser::model::{
    Argument, Class, Dirent, Dynamic, Expression, Listing, ListingEntry, LoadListing,
    OutputBinding, OutputParameter, Process, RequirementClass, SecondaryFile, Type, Version,
    short_name,
};
use workflow_graph::mock::glob_matches;
use workflow_graph::{Input, Outcome, OutputFile, Resources, Task, TaskId};

use crate::binding::{self, Binder, Bound, Key};
use crate::expression::{Evaluator, Variables};
use crate::files::{self, Access};
use crate::inputs::has_code;
use crate::scope::Scope;
use crate::types::{Hint, Types};
use crate::value::{Map, Value};

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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mock: true,
            image: DEFAULT_IMAGE.into(),
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
/// input object.
pub fn task(
    process: &Arc<Process>,
    job: Map,
    scope: &Scope,
    options: &Options,
    id: TaskId,
    names: (&str, &str),
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
    let (resources, sizes) = resources(scope, &evaluator, &job, &runtime, process.version)?;
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
            match evaluator.evaluate(expression, vars)? {
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
                            let value = evaluator.evaluate(expression, vars)?;
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
            Some(expression) => evaluator.evaluate(
                expression,
                Variables {
                    this: &bound.value,
                    ..vars
                },
            )?,
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
        (args.into_iter().map(|arg| arg.text).collect(), None)
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
            let value = evaluator.evaluate(&variable.env_value, vars)?;
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
                )?;
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
        Some((Dynamic::Value(seconds), _)) => u64::try_from(*seconds).ok(),
        Some((Dynamic::Expression(expression), _)) => evaluator
            .evaluate(expression, vars)?
            .as_i64()
            .and_then(|seconds| u64::try_from(seconds).ok()),
        None => None,
    }
    .filter(|seconds| *seconds > 0);
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
        let value = evaluator.evaluate(expression, vars)?;
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

/// Chooses where every input file goes inside the container.
#[derive(Default)]
struct Stager {
    inputs: Vec<Input>,
    /// Container paths by location.
    paths: HashMap<String, String>,
    folders: usize,
    /// The output directory, where relative task input paths are.
    outdir: String,
    /// The locations of the files of the input object.
    from_job: HashSet<String>,
    /// The locations that InitialWorkDir copies as writable.
    writable: HashSet<String>,
}

impl Stager {
    /// Stages every file object of an input object, each top-level one in a folder of its own.
    fn stage_job(&mut self, job: &Value) {
        match job {
            Value::List(items) => {
                for item in items {
                    self.stage_job(item);
                }
            }
            Value::Map(map) if job.is_file_like() => {
                let location = map
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.paths.contains_key(location) {
                    return;
                }
                let folder = format!("{STAGING}/{}", self.folders);
                self.folders += 1;
                self.stage(map, &folder, false);
                let mut file = job.clone();
                files::visit_files(&mut file, &mut |inner| {
                    if let Some(Value::String(location)) = inner.get("location") {
                        self.from_job.insert(location.clone());
                    }
                });
            }
            Value::Map(map) => {
                for (_, value) in map {
                    self.stage_job(value);
                }
            }
            _ => {}
        }
    }

    /// Stages a file object into a folder, with its secondary files beside it and the
    /// entries of a literal directory inside it. `replace` lets the new path win.
    fn stage(&mut self, file: &Map, folder: &str, replace: bool) {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let basename = file
            .get("basename")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| files::basename_of(&location));
        let target = join(folder, &basename);
        if replace || !self.paths.contains_key(&location) {
            let inside = self.container_path(&target);
            self.paths.insert(location.clone(), inside);
        }
        let directory = file.get("class").and_then(Value::as_str) == Some("Directory");
        let literal = location.starts_with("_:");
        if directory {
            if literal {
                self.add(Input {
                    path: target.clone(),
                    source: String::new(),
                    directory: true,
                    content: None,
                });
            } else {
                self.add(Input {
                    path: target.clone(),
                    source: source_of(&location),
                    directory: true,
                    content: None,
                });
            }
            if let Some(Value::List(listing)) = file.get("listing") {
                for entry in listing {
                    let Value::Map(entry) = entry else { continue };
                    let child = entry
                        .get("location")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match child.strip_prefix(&format!("{location}/")) {
                        Some(relative) if !literal => {
                            self.inside(entry, &join(&target, relative), replace);
                        }
                        _ => self.stage(entry, &target, replace),
                    }
                }
            }
        } else if literal {
            let content = file
                .get("contents")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.add(Input {
                path: target.clone(),
                source: String::new(),
                directory: false,
                content: Some(content.to_owned()),
            });
        } else {
            self.add(Input {
                path: target.clone(),
                source: source_of(&location),
                directory: false,
                content: None,
            });
        }
        if let Some(Value::List(secondary)) = file.get("secondaryFiles") {
            for entry in secondary {
                if let Value::Map(entry) = entry {
                    self.stage(entry, folder, replace);
                }
            }
        }
    }

    /// Records the path of an entry that a staged directory already holds.
    fn inside(&mut self, entry: &Map, target: &str, replace: bool) {
        let location = entry
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if replace || !self.paths.contains_key(location) {
            let inside = self.container_path(target);
            self.paths.insert(location.to_owned(), inside);
        }
        if let Some(Value::List(listing)) = entry.get("listing") {
            for child in listing {
                let Value::Map(child) = child else { continue };
                let name = child
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(relative) = name.strip_prefix(&format!("{location}/")) {
                    self.inside(child, &join(target, relative), replace);
                }
            }
        }
    }

    /// Task input paths are relative to the workdir, the output directory, or absolute.
    fn container_path(&self, target: &str) -> String {
        if target.starts_with('/') {
            target.to_owned()
        } else {
            format!("{}/{target}", self.outdir)
        }
    }

    fn add(&mut self, input: Input) {
        if let Some(existing) = self
            .inputs
            .iter_mut()
            .find(|existing| existing.path == input.path)
        {
            *existing = input;
        } else {
            self.inputs.push(input);
        }
    }
}

/// A path inside a folder; `.` and `..` of the name are kept as written.
fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() || name.starts_with('/') {
        name.to_owned()
    } else {
        format!("{}/{name}", folder.trim_end_matches('/'))
    }
}

/// Where a task reads a location from: a local path, or the URL.
fn source_of(location: &str) -> String {
    match files::local_path(location) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => location.to_owned(),
    }
}

/// Sets `path`, `dirname`, `basename`, `nameroot` and `nameext` of every staged file object
/// to its place inside the container, as cwltool's `check_adjust` does.
fn adjust(value: &mut Value, paths: &HashMap<String, String>) {
    files::visit_files(value, &mut |file| {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(path) = paths.get(location) else {
            return;
        };
        let path = path.clone();
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", &path));
        file.insert("dirname".into(), Value::string(folder));
        file.insert("basename".into(), Value::string(name));
        if file.get("class").and_then(Value::as_str) == Some("File") {
            let (root, extension) = files::split_extension(name);
            file.insert("nameroot".into(), Value::string(root));
            file.insert("nameext".into(), Value::string(extension));
        }
        file.insert("path".into(), Value::String(path));
    });
}
