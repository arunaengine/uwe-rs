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

/// Evaluates `InitialWorkDirRequirement` and stages its entries in the output directory.
fn initial_workdir(
    process: &Process,
    scope: &Scope,
    evaluator: &Evaluator,
    inputs: &Value,
    runtime: &Map,
    stager: &mut Stager,
) -> Result<(), String> {
    let Some((listing, _)) = scope.find(|class| match class {
        RequirementClass::InitialWorkDir { listing } => Some(listing),
        _ => None,
    }) else {
        return Ok(());
    };
    let runtime = Value::Map(runtime.clone());
    let vars = Variables {
        inputs,
        this: &Value::Null,
        runtime: &runtime,
    };
    let mut entries: Vec<Value> = Vec::new();
    match listing {
        Listing::Expression(expression) => match evaluator.evaluate(expression, vars)? {
            Value::List(items) => {
                for item in items {
                    match item {
                        Value::Null => {}
                        Value::List(nested) => entries.extend(nested),
                        other => entries.push(other),
                    }
                }
            }
            Value::Null => {}
            other => {
                return Err(format!(
                    "the InitialWorkDirRequirement listing must give a list, not {}",
                    other.to_json()
                ));
            }
        },
        Listing::Entries(items) => {
            for item in items {
                match item {
                    ListingEntry::Dirent(dirent) => {
                        entries.extend(dirent_entries(dirent, evaluator, vars)?);
                    }
                    ListingEntry::Expression(expression) => {
                        match evaluator.evaluate(expression, vars)? {
                            Value::Null => {}
                            Value::List(items) => {
                                entries.extend(items.into_iter().filter(|item| !item.is_null()))
                            }
                            other if !other.is_truthy() => {}
                            other => entries.push(other),
                        }
                    }
                    ListingEntry::Object(node) => match Value::from_node(node) {
                        Value::List(items) => entries.extend(items),
                        other => entries.push(other),
                    },
                }
            }
        }
    }
    for entry in entries {
        let Value::Map(mut map) = entry else {
            return Err(format!(
                "an InitialWorkDirRequirement entry must be a file, a directory or a Dirent, not {}",
                entry.to_json()
            ));
        };
        // Dirents left by the expressions become files or renamed file objects.
        if map.contains_key("entry") {
            let entryname = map
                .get("entryname")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let writable = map.get("writable").cloned();
            let entry = map.shift_remove("entry").unwrap_or(Value::Null);
            map = match entry {
                Value::Map(mut object) if object.contains_key("class") => {
                    if let Some(name) = entryname {
                        object.insert("basename".into(), Value::String(name));
                    }
                    object
                }
                Value::Null => continue,
                other => {
                    let Some(name) = entryname else {
                        return Err("an InitialWorkDirRequirement entry has no entryname".into());
                    };
                    let contents = match other {
                        Value::String(text) => text,
                        other => other.to_python_json(true),
                    };
                    let mut file = Map::new();
                    file.insert("class".into(), Value::string("File"));
                    file.insert("basename".into(), Value::String(name));
                    file.insert("contents".into(), Value::String(contents));
                    file
                }
            };
            if let Some(writable) = writable {
                map.insert("writable".into(), writable);
            }
        }
        if !matches!(
            map.get("class").and_then(Value::as_str),
            Some("File" | "Directory")
        ) {
            return Err(format!(
                "an InitialWorkDirRequirement entry must be a file or a directory, not {}",
                Value::Map(map).to_json()
            ));
        }
        let mut value = Value::Map(map);
        files::normalize(&mut value)?;
        let Value::Map(map) = value else { continue };
        let basename = map
            .get("basename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut basename = normalize_path(&basename);
        // An absolute name inside the output directory, like `$(runtime.outdir)/x`, is `x`.
        if let Some(inside) = basename.strip_prefix(&format!("{}/", stager.outdir)) {
            basename = inside.to_owned();
        }
        if basename.starts_with("../") || basename == ".." {
            return Err(format!(
                "the InitialWorkDir entry `{basename}` leaves the output directory"
            ));
        }
        if basename.starts_with('/') && process.version < Version::V1_2Dev4 {
            return Err(format!(
                "the InitialWorkDir entry `{basename}` is absolute, which needs cwlVersion v1.2"
            ));
        }
        let (folder, name) = match basename.rsplit_once('/') {
            Some(("", name)) => ("/".to_owned(), name.to_owned()),
            Some((folder, name)) => (folder.to_owned(), name.to_owned()),
            None => (String::new(), basename.clone()),
        };
        let mut map = map;
        map.insert("basename".into(), Value::String(name));
        stager.stage(&map, &folder, true);
        if map.get("writable") == Some(&Value::Bool(true)) {
            let mut entry = Value::Map(map);
            files::visit_files(&mut entry, &mut |inner| {
                if let Some(Value::String(location)) = inner.get("location") {
                    stager.writable.insert(location.clone());
                }
            });
        }
    }
    Ok(())
}

/// The entries of one Dirent: a file with literal contents, a renamed file object, or the
/// file objects of a list.
fn dirent_entries(
    dirent: &Dirent,
    evaluator: &Evaluator,
    vars: Variables<'_>,
) -> Result<Vec<Value>, String> {
    let entry = evaluator.evaluate_untrimmed(&dirent.entry, vars)?;
    let entryname = match &dirent.entryname {
        Some(expression) => match evaluator.evaluate(expression, vars)? {
            Value::String(name) => Some(name),
            other => {
                return Err(format!(
                    "`entryname` must be a string, not {}",
                    other.to_json()
                ));
            }
        },
        None => None,
    };
    let writable = Value::Bool(dirent.writable);
    match entry {
        Value::Null => Ok(Vec::new()),
        Value::List(items) if !items.is_empty() && items.iter().all(Value::is_file_like) => {
            if entryname.is_some() {
                return Err("`entryname` cannot name a list of files".into());
            }
            Ok(items
                .into_iter()
                .map(|mut item| {
                    if let Value::Map(map) = &mut item {
                        map.insert("writable".into(), writable.clone());
                    }
                    item
                })
                .collect())
        }
        entry => {
            let mut dirent_map = Map::new();
            dirent_map.insert("entry".into(), entry);
            if let Some(name) = entryname {
                dirent_map.insert("entryname".into(), Value::String(name));
            }
            dirent_map.insert("writable".into(), writable);
            Ok(vec![Value::Map(dirent_map)])
        }
    }
}

/// A relative path without `.` segments and with `..` folded, as `os.path.normpath` does.
fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
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
                                other.to_json()
                            ));
                        }
                    }
                }
                texts
            }
            other => return Err(format!("a glob must be a string, not {}", other.to_json())),
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
                    let placeholder = self.placeholder(&output.value_type, &name, &collector);
                    outputs.insert(name, placeholder);
                } else {
                    return Err(format!(
                        "the output `{name}` is not a valid {}: {}",
                        crate::inputs::describe(&output.value_type),
                        value.to_json()
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
    fn placeholder(&self, value_type: &Type, name: &str, collector: &Collector<'_>) -> Value {
        let base = match &collector.workdir {
            Some(folder) => format!("{}/", iri::file_url(Path::new(folder))),
            None => "_:".to_owned(),
        };
        let mut value = self
            .types
            .placeholder(value_type, name, &base, false, &Hint::default());
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
        let mut found: Vec<Value> = Vec::new();
        for pattern in &patterns {
            for file in self.matches(pattern) {
                found.push(self.file_object(&file, &binding)?);
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
                let value = self.evaluate(format, primary)?;
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
}
