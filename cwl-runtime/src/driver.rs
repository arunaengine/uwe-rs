//! The CWL driver of the execution graph: it loads a document and its input object, starts
//! the main process, and turns outcomes of tasks into more tasks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cwl_parser::iri;
use cwl_parser::model::record::Context;
use cwl_parser::model::{
    Class, Process, Requirement, RequirementClass, Type, Version, read_requirements, short_name,
};
use cwl_parser::salad::context::Namespaces;
use cwl_parser::source::Remote;
use storage::Storage;
use workflow_graph::{
    Delivery, Driver, DriverError, Files, Inputs, Language, Launch, Note, Outcome, Task, TaskId,
};

use crate::files;
use crate::results;
use crate::run::Run;
use crate::schema;
use crate::tool::{Options, UNSUPPORTED};
use crate::types::Types;
use crate::value::{Map, Value};

/// The namespace of cwltool's extensions, whose hints it knows.
const CWLTOOL: &str = "http://commonwl.org/cwltool#";

/// Runs one CWL document with one input object.
pub struct CwlDriver {
    launch: Launch,
    /// Whether the run is simulated, so that its output object names made-up files.
    simulated: bool,
    /// The files of the output object to deliver, once the run finished for real.
    deliveries: Vec<Delivery>,
    process: Arc<Process>,
    /// The `$namespaces` of the document, which expand prefixed formats of the job file.
    namespaces: Namespaces,
    run: Run,
    /// Why the tools that failed failed, in the order they ended. After the first, no step
    /// starts unless the run keeps going, and the run fails at its end, as cwltool does.
    failures: Vec<String>,
    acceptance: HashSet<(TaskId, u32)>,
    /// What the run ignores, like hints of unknown classes; printed as they are found.
    warnings: Vec<String>,
}

impl CwlDriver {
    /// Loads the document of a launch. `Launch.targets` may name the process of a packed
    /// document, like `main`.
    pub fn new(launch: &Launch, options: Options) -> Result<Self, DriverError> {
        let mut reference = launch.target.to_string_lossy().into_owned();
        if let Some(target) = launch.targets.first()
            && !reference.contains('#')
        {
            reference = format!("{reference}#{}", target.trim_start_matches('#'));
        }
        let document = cwl_parser::load_file(&reference, options.remote.clone())
            .map_err(|error| DriverError::new(error.to_string()))?;
        let simulated = options.simulated;
        let mut run = Run::new(options, launch.keep_going);
        run.name_ontologies(document.schemas);
        let mut warnings = Vec::new();
        unknown_hints(&document.process, &mut warnings);
        for warning in &warnings {
            eprintln!("WARN: {warning}");
        }
        Ok(CwlDriver {
            launch: launch.clone(),
            simulated,
            deliveries: Vec::new(),
            run,
            namespaces: Namespaces::default().extend(document.namespaces),
            process: document.process,
            failures: Vec::new(),
            acceptance: HashSet::new(),
            warnings,
        })
    }

    /// What the run ignores, like hints of unknown classes.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The output object of the run, once it finished.
    pub fn outputs(&self) -> Option<&Map> {
        self.run.outputs()
    }

    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    /// The input object from the job file and the parameters. Without a job file, the run
    /// gives required inputs that nothing sets placeholders from their types.
    pub fn input_object(&self) -> Result<Map, String> {
        let mut job = match &self.launch.inputs {
            Some(path) => read_job(path, &self.namespaces)?,
            None => Map::new(),
        };
        let types = self.types();
        let mut given: Vec<(String, Vec<String>)> = Vec::new();
        for (name, text) in &self.launch.params {
            match given.iter_mut().find(|(known, _)| known == name) {
                Some((_, texts)) => texts.push(text.clone()),
                None => given.push((name.clone(), vec![text.clone()])),
            }
        }
        for (name, texts) in given {
            let Some(input) = self
                .process
                .inputs
                .iter()
                .find(|input| short_name(&input.id) == name)
            else {
                return Err(format!("the process has no input `{name}`"));
            };
            let value = parameter(&types, &input.value_type, &texts)?;
            job.insert(name, value);
        }
        Ok(job)
    }
}

impl CwlDriver {
    /// The named types of the process: those of its `SchemaDefRequirement` and of its inputs.
    fn types(&self) -> Types {
        let mut types = Types::default();
        for requirement in self.process.requirements.iter().chain(&self.process.hints) {
            if let cwl_parser::model::RequirementClass::SchemaDef { types: defined } =
                &requirement.class
            {
                for value_type in defined {
                    types.add(value_type);
                }
            }
        }
        for input in &self.process.inputs {
            types.add(&input.value_type);
        }
        types
    }
}

/// The driver of a CWL launch without the providers of a config file: it takes absent files
/// as present and empty when the launch is simulated.
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError> {
    let options = Options {
        simulated: launch.simulated,
        ..Options::default()
    };
    Ok(Box::new(CwlDriver::new(launch, options)?))
}

/// The driver of a CWL launch that reads documents and files at addresses through `storage`,
/// which keeps what it downloads in the folder `downloads`.
pub fn driver_with(
    launch: &Launch,
    storage: &Storage,
    downloads: PathBuf,
) -> Result<Box<dyn Driver>, DriverError> {
    driver_with_options(launch, storage, downloads, Options::default())
}

/// Runs with explicit JavaScript helper settings and the launch's storage access.
pub fn driver_with_options(
    launch: &Launch,
    storage: &Storage,
    downloads: PathBuf,
    mut options: Options,
) -> Result<Box<dyn Driver>, DriverError> {
    options.simulated = launch.simulated;
    options.remote = Remote::new(storage, downloads);
    Ok(Box::new(CwlDriver::new(launch, options)?))
}

/// Notes the hints of a process and of its steps whose class neither CWL nor cwltool knows,
/// which the run ignores, as cwltool says with `Unknown hint`.
fn unknown_hints(process: &Process, warnings: &mut Vec<String>) {
    let note = |hints: &[Requirement], warnings: &mut Vec<String>| {
        for hint in hints {
            let RequirementClass::Other { class, .. } = &hint.class else {
                continue;
            };
            let warning = format!("{}: Unknown hint {class}, which is ignored", hint.location);
            if !class.starts_with(CWLTOOL) && !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    };
    note(&process.hints, warnings);
    if let Class::Workflow(workflow) = &process.class {
        for step in &workflow.steps {
            note(&step.hints, warnings);
            unknown_hints(&step.run.process, warnings);
        }
    }
}

/// Where the first abstract `Operation` of a process or of its steps is.
fn abstract_operation(process: &Process) -> Option<&cwl_parser::Location> {
    match &process.class {
        Class::Operation => Some(&process.location),
        Class::Workflow(workflow) => {
            (workflow.steps.iter()).find_map(|step| abstract_operation(&step.run.process))
        }
        _ => None,
    }
}

/// A driver error from a message; messages of unsupported features give exit code 33.
fn error(message: String) -> DriverError {
    match message.find(UNSUPPORTED) {
        Some(start) => {
            let mut text = message.clone();
            text.replace_range(start..start + UNSUPPORTED.len(), "");
            DriverError::unsupported(text)
        }
        None => DriverError::new(message),
    }
}

impl Driver for CwlDriver {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        // cwltool refuses the whole run, even for a step that `when` would skip; a simulated
        // run plans the operation as a task that does nothing, for graphs.
        if !self.simulated
            && let Some(at) = abstract_operation(&self.process)
        {
            let message = format!("{at}: Workflow has unrunnable abstract Operation");
            return Err(DriverError::new(message));
        }
        let job = self.input_object().map_err(error)?;
        // Without a job file, required inputs that nothing sets get placeholders.
        if self.launch.inputs.is_none() {
            let base = placeholder_base(&self.launch, &self.process);
            self.run.use_placeholders(&base);
        }
        let requirements = match &self.launch.inputs {
            Some(path) => job_requirements(path, self.process.version).map_err(error)?,
            None => Vec::new(),
        };
        let name = process_name(&self.process.id);
        let process = Arc::clone(&self.process);
        (self.run.start(&process, job, &name, requirements)).map_err(error)
    }

    /// A tool that failed fails its step.
    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let stopping = !self.failures.is_empty() && !self.launch.keep_going;
        let identity = (outcome.id, outcome.attempt);
        let completed = self.run.complete(&outcome);
        if completed.is_ok() {
            self.acceptance.insert(identity);
        }
        match completed {
            Err(message) if message.contains(UNSUPPORTED) => Err(error(message)),
            Err(message) => {
                self.failures.push(message);
                Ok(match self.launch.keep_going {
                    true => self.run.take_ready(),
                    false => Vec::new(),
                })
            }
            Ok(_) if stopping => Ok(Vec::new()),
            Ok(tasks) => Ok(tasks),
        }
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.acceptance.remove(&(task, attempt))
    }

    fn notes(&mut self) -> Vec<Note> {
        self.run.take_notes()
    }

    fn draining(&self) -> bool {
        !self.failures.is_empty() && !self.launch.keep_going
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.run.use_files(files);
    }

    fn use_cancel(&mut self, cancel: workflow_graph::CancellationToken) {
        self.run.use_cancel(cancel);
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    /// The typed inputs of the process and its input object. Files and directories in the
    /// object are absolute paths or addresses already. Without a job file, the run makes up the
    /// required inputs that nothing sets.
    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        let job = self.input_object().map_err(error)?;
        Ok(Some(Inputs {
            schema: schema::of(&self.process, &self.types(), &self.namespaces)
                .map_err(DriverError::new)?,
            values: schema::values(&job).map_err(DriverError::new)?,
            base: std::env::current_dir().unwrap_or_default(),
            read: Some(schema::read(&self.process)),
            assumes_values: self.launch.inputs.is_none(),
            advisory: false,
            ignored: Default::default(),
        }))
    }

    /// The typed outputs of the process: the output object of a run.
    fn outputs(&mut self) -> Result<Option<serde_json::Value>, DriverError> {
        let schema = schema::outputs(&self.process, &self.types(), &self.namespaces);
        Ok(Some(schema))
    }

    /// A real run delivers the files of the output object to the output folder and prints
    /// the object, which then names them there, as `cwltool --outdir` does.
    fn finish(&mut self) -> Result<(), DriverError> {
        if let Some(first) = self.failures.first() {
            let message = match self.failures.len() {
                1 => first.clone(),
                count => format!("{first}; and {} more steps failed", count - 1),
            };
            return Err(DriverError::new(message));
        }
        if let Some(outputs) = self.run.outputs() {
            if !self.simulated {
                let folder = match &self.launch.output_dir {
                    // An address of a storage provider, like `s3://bucket/out`, stays as it is.
                    Some(folder) if folder.to_string_lossy().contains("://") => folder.clone(),
                    Some(folder) => std::path::absolute(folder).unwrap_or(folder.clone()),
                    None => std::env::current_dir().unwrap_or_default(),
                };
                let mut outputs = outputs.clone();
                self.deliveries = results::deliver(&mut outputs, &folder).map_err(error)?;
                println!("{}", Value::Map(outputs).to_pretty_json());
            }
            return Ok(());
        }
        let waiting = self.run.waiting();
        Err(DriverError::new(if waiting.is_empty() {
            "the run ended before its process finished".to_owned()
        } else {
            format!(
                "these steps wait for inputs that never came: {}",
                waiting.join(", ")
            )
        }))
    }
}

/// A short name for the main process: its fragment, or the file name without extension.
pub fn process_name(id: &str) -> String {
    if let Some((_, fragment)) = id.split_once('#') {
        return short_name(fragment).to_owned();
    }
    let name = files::basename_of(id);
    files::split_extension(&name).0.to_owned()
}

/// Reads a job file, with the locations in it relative to the file, and the namespace
/// prefixes of file formats expanded.
fn read_job(path: &Path, namespaces: &Namespaces) -> Result<Map, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut value = Value::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    files::resolve_locations(&mut value, &iri::file_url(&absolute));
    files::visit_files(&mut value, &mut |file| {
        if let Some(Value::String(format)) = file.get_mut("format") {
            *format = namespaces.expand_prefix(format);
        }
    });
    match value {
        Value::Map(mut map) => {
            // Requirements of the job, not an input (see `job_requirements`).
            map.shift_remove(REQUIREMENTS);
            Ok(map)
        }
        Value::Null => Ok(Map::new()),
        other => Err(format!(
            "{} must hold an object, not {}",
            path.display(),
            other.to_json()
        )),
    }
}

/// The key of the input object that holds requirements.
const REQUIREMENTS: &str = "cwl:requirements";

/// The requirements that an input object gives under `cwl:requirements`, which override
/// those of every process of the run, as cwltool reads them. Their fields are lists, as a
/// document has them after Salad.
fn job_requirements(path: &Path, version: Version) -> Result<Vec<Requirement>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let file: Arc<str> = Arc::from(path.to_string_lossy().as_ref());
    let node = cwl_parser::yaml::read(&text, &file).map_err(|error| error.to_string())?;
    let context = Context { version };
    let requirements = read_requirements(node.get(REQUIREMENTS), &context, false);
    requirements.map_err(|error| error.to_string())
}

/// The value of an input given on the command line, read by its type; repeated values make
/// a list.
fn parameter(types: &Types, value_type: &Type, texts: &[String]) -> Result<Value, String> {
    let resolved = types.resolve(value_type);
    match resolved {
        Type::Union(members) => {
            let mut last = Err("no type fits".to_owned());
            for member in members
                .iter()
                .filter(|member| !matches!(member, Type::Null))
            {
                last = parameter(types, member, texts);
                if last.is_ok() {
                    return last;
                }
            }
            last
        }
        Type::Array(array) => texts
            .iter()
            .map(|text| parameter(types, &array.items, std::slice::from_ref(text)))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        _ => {
            let text = texts.last().map(String::as_str).unwrap_or_default();
            match resolved {
                Type::Boolean => match text {
                    "true" | "True" | "" => Ok(Value::Bool(true)),
                    "false" | "False" => Ok(Value::Bool(false)),
                    _ => Err(format!("`{text}` is not a boolean")),
                },
                Type::Int | Type::Long => text
                    .parse()
                    .map(Value::Integer)
                    .map_err(|_| format!("`{text}` is not an integer")),
                Type::Float | Type::Double => text
                    .parse()
                    .map(Value::Float)
                    .map_err(|_| format!("`{text}` is not a number")),
                Type::File | Type::Directory => {
                    let class = if matches!(resolved, Type::File) {
                        "File"
                    } else {
                        "Directory"
                    };
                    let absolute = std::path::absolute(text).unwrap_or_else(|_| text.into());
                    let location = if iri::has_scheme(text) {
                        text.to_owned()
                    } else {
                        iri::file_url(&absolute)
                    };
                    let mut map = Map::new();
                    map.insert("class".into(), Value::string(class));
                    map.insert("location".into(), Value::String(location));
                    Ok(Value::Map(map))
                }
                Type::Record(_) | Type::Any => {
                    Value::parse(text).or_else(|_| Ok(Value::string(text)))
                }
                _ => Ok(Value::string(text)),
            }
        }
    }
}

/// The folder URL placeholder input files are named in: `inputs/` of the run's scratch
/// folder, or the folder of the document.
fn placeholder_base(launch: &Launch, process: &Process) -> String {
    if !launch.scratch.as_os_str().is_empty() {
        let folder = std::path::absolute(launch.scratch.join("inputs"))
            .unwrap_or_else(|_| launch.scratch.join("inputs"));
        return format!("{}/", iri::file_url(&folder));
    }
    let document = iri::split_fragment(&process.id).0;
    match document.rfind('/') {
        Some(slash) => document[..=slash].to_owned(),
        None => "file:///".to_owned(),
    }
}
