//! The driver of a wir document: it starts each op of the entry graph once its inputs are
//! bound and its dependencies hold, and binds the outputs of the ops that succeed.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value as Json};
use wir::{Document, Kind, Language as Dialect, Native, Status};
use workflow_graph::{
    CancellationToken, Delivery, DeliveryMode, Driver, DriverError, ExecuteError, Files, Language,
    Launch, Note, Outcome, Reuse, Task, TaskId,
};

use crate::child::Child;
use crate::cwl;
use crate::inputs;
use crate::nextflow;
use crate::plan::plan;
use crate::snakemake;
use crate::state::{Progress, widen};
use crate::task;

/// How a run makes the drivers of native blocks, and what it shows.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The options of CWL blocks. A block never prints its output object, and the launch
    /// says whether it is simulated.
    pub cwl: cwl_runtime::Options,
    /// How the drivers of Nextflow blocks get their settings, with a launch folder each.
    pub nextflow: nextflow::Options,
    /// What the drivers of Snakemake blocks use; each starts in a folder of its own.
    pub snakemake: snakemake::Options,
    /// Whether a real run prints the outputs of its graph, as JSON.
    pub print: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cwl: cwl_runtime::Options::default(),
            nextflow: nextflow::Options::default(),
            snakemake: snakemake::Options::default(),
            print: true,
        }
    }
}

impl Settings {
    /// Reads CWL documents and files at addresses through `storage`, which keeps what it
    /// downloads in `downloads`.
    pub fn with_storage(mut self, storage: &storage::Storage, downloads: PathBuf) -> Self {
        self.cwl.remote = cwl_parser::source::Remote::new(storage, downloads);
        self
    }
}

/// The driver of a wir launch: parses the document, verifies it, then runs it.
pub fn driver(launch: &Launch, settings: Settings) -> Result<Box<dyn Driver>, DriverError> {
    Ok(Box::new(WirDriver::new(launch, settings)?))
}

/// Runs the entry graph of one wir document.
pub struct WirDriver {
    launch: Launch,
    settings: Settings,
    document: Document,
    /// The folder of the document, which relative source locations start from.
    base: PathBuf,
    progress: Progress,
    /// The native blocks that started, with the node of each.
    children: Vec<(usize, Child)>,
    /// The child of each task, by its id in this run.
    owners: HashMap<TaskId, usize>,
    /// The launch folder and the input values of each Nextflow block, by node.
    launched: HashMap<usize, (PathBuf, Map<String, Json>)>,
    /// The tasks of task ops, by id, with the node of each.
    tasks: HashMap<TaskId, (usize, Task)>,
    /// The attempts of task ops that their captures accepted.
    accepted: HashSet<(TaskId, u32)>,
    last: u64,
    files: Option<Arc<dyn Files>>,
    cancel: Option<CancellationToken>,
    /// Where the job files of native blocks are written.
    folder: Option<tempfile::TempDir>,
    results: Option<Json>,
    /// The files of the graph outputs that task ops made, to deliver to the output folder.
    delivering: Vec<Delivery>,
}

impl WirDriver {
    pub fn new(launch: &Launch, settings: Settings) -> Result<Self, DriverError> {
        let shown = launch.target.display();
        let text = std::fs::read_to_string(&launch.target)
            .map_err(|error| DriverError::new(format!("cannot read {shown}: {error}")))?;
        let document =
            wir::parse(&text).map_err(|error| DriverError::new(format!("{shown}: {error}")))?;
        if let Err(problems) = wir::verify(&document) {
            let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
            return Err(DriverError::new(format!(
                "{shown}: {}",
                problems.join("; ")
            )));
        }
        let progress = Progress::new(plan(&document)?, document.types.clone());
        let target = std::path::absolute(&launch.target).unwrap_or(launch.target.clone());
        Ok(WirDriver {
            launch: launch.clone(),
            settings,
            base: target.parent().unwrap_or(Path::new("/")).to_owned(),
            document,
            progress,
            children: Vec::new(),
            owners: HashMap::new(),
            launched: HashMap::new(),
            tasks: HashMap::new(),
            accepted: HashSet::new(),
            last: 0,
            files: None,
            cancel: None,
            folder: None,
            results: None,
            delivering: Vec::new(),
        })
    }

    /// Starts every node that can start; the tasks they ask for.
    fn advance(&mut self) -> Result<Vec<Task>, DriverError> {
        let mut tasks = Vec::new();
        loop {
            let draining = self.draining();
            let Some((at, bound)) = self.progress.next(draining) else {
                return Ok(tasks);
            };
            tasks.extend(self.begin(at, bound)?);
        }
    }

    /// Starts a node with its input values; the tasks it asks for.
    fn begin(&mut self, at: usize, bound: Map<String, Json>) -> Result<Vec<Task>, DriverError> {
        match self.progress.node(at).op.kind.clone() {
            Kind::Native(native) => match self.start_native(at, &native, bound) {
                Ok(tasks) => Ok(tasks),
                Err(error) if error.unsupported => Err(error),
                Err(error) => {
                    let message = format!("op {}: {}", self.progress.node(at).path, error.message);
                    self.progress
                        .end(at, Status::Errored, Map::new(), Some(message));
                    Ok(Vec::new())
                }
            },
            Kind::Task(spec) => {
                self.last += 1;
                let node = self.progress.node(at);
                let task = task::build(TaskId(self.last), &node.path, &node.op, &spec, &bound);
                self.tasks.insert(task.id, (at, task.clone()));
                Ok(vec![task])
            }
            Kind::Call(_) => Ok(Vec::new()),
            _ => {
                let path = &self.progress.node(at).path;
                Err(DriverError::unsupported(format!(
                    "op {path}: uwe does not run this op kind yet"
                )))
            }
        }
    }

    /// Reads the outputs of a task op from its outcome. An attempt that is not accepted runs
    /// again while the op's `retry` allows it; then the op fails, or errors when the task
    /// could not run.
    fn complete_task(&mut self, outcome: &Outcome) -> Vec<Task> {
        let (at, task) = &self.tasks[&outcome.id];
        let (at, node) = (*at, self.progress.node(*at));
        let Kind::Task(spec) = &node.op.kind else {
            return Vec::new();
        };
        match task::capture(spec, &node.op.outputs, outcome, &self.document.types) {
            Ok(values) => {
                self.accepted.insert((outcome.id, outcome.attempt));
                self.progress.end(at, Status::Succeeded, values, None);
                Vec::new()
            }
            Err(_) if outcome.attempt <= node.op.attributes.retry.unwrap_or(0) => {
                vec![task.clone()]
            }
            Err(why) => {
                let status = match outcome.error {
                    Some(_) => Status::Errored,
                    None => Status::Failed,
                };
                let message = format!("op {}: {why}", node.path);
                self.progress.end(at, status, Map::new(), Some(message));
                Vec::new()
            }
        }
    }

    /// Starts the language driver of a native block as a child, with a job of the node's
    /// input values and a working folder of its own.
    fn start_native(
        &mut self,
        at: usize,
        native: &Native,
        bound: Map<String, Json>,
    ) -> Result<Vec<Task>, DriverError> {
        let path = self.progress.node(at).path.clone();
        let target = self.locate(&native.source)?;
        let configs = (native.configs.iter())
            .map(|id| self.locate(id))
            .collect::<Result<Vec<_>, _>>()?;
        let folder = self.job_folder()?.join(&path);
        let (job, values) = match native.language {
            Dialect::Nextflow => (folder.join("params.json"), nextflow::params(&bound)),
            // A Snakemake config file takes the same values as Nextflow params.
            Dialect::Snakemake => (folder.join("config.json"), nextflow::params(&bound)),
            _ => (
                folder.join("job.json"),
                cwl::to_cwl(Json::Object(bound.clone())),
            ),
        };
        let written = std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::write(&job, values.to_string()));
        written.map_err(|error| {
            DriverError::new(format!("cannot write {}: {error}", job.display()))
        })?;
        let output = self.output_root().join(&path);
        let mut launch = Launch {
            target,
            inputs: Some(job),
            profiles: native.profiles.clone(),
            configs,
            targets: native.entry.iter().cloned().collect(),
            scratch: self.launch.scratch.clone(),
            output_dir: Some(output.clone()),
            stub: self.launch.stub,
            simulated: self.launch.simulated,
            keep_going: self.launch.keep_going,
            ..Launch::default()
        };
        let mut child = Child::new(match native.language {
            Dialect::Cwl if !launch.profiles.is_empty() || !launch.configs.is_empty() => {
                let message = format!("op {path}: CWL blocks take no profiles or configs");
                return Err(DriverError::unsupported(message));
            }
            Dialect::Cwl => cwl::block(&launch, &self.settings.cwl)?,
            // A Nextflow block runs as if launched in its output folder, and keeps its own
            // files in a scratch folder of its own, which its driver removes.
            Dialect::Nextflow => {
                (launch.output_dir, launch.scratch) = (None, folder);
                let driver = nextflow::block(&launch, &output, &self.settings.nextflow)?;
                self.launched.insert(at, (output, bound));
                driver
            }
            // A Snakefile block starts in its output folder, a project folder in itself.
            Dialect::Snakemake => {
                std::fs::create_dir_all(&output).map_err(|error| {
                    DriverError::new(format!("cannot make {}: {error}", output.display()))
                })?;
                snakemake::block(&launch, &output, &self.settings.snakemake)?
            }
            other => {
                let message = format!("op {path}: uwe does not run {other:?} blocks yet");
                return Err(DriverError::unsupported(message));
            }
        });
        child.prepare(
            self.files.clone(),
            self.cancel.clone(),
            self.launch.simulated,
        )?;
        let last = &mut self.last;
        let tasks = child.start(&mut || {
            *last += 1;
            TaskId(*last)
        })?;
        self.children.push((at, child));
        Ok(self.adopt(self.children.len() - 1, tasks))
    }

    /// Records the owner of tasks a child asked for, under the op path, and ends the child
    /// once it has no work left.
    fn adopt(&mut self, index: usize, mut tasks: Vec<Task>) -> Vec<Task> {
        let (at, child) = &mut self.children[index];
        let node = self.progress.node(*at);
        for task in &mut tasks {
            self.owners.insert(task.id, index);
            task.name = format!("{}: {}", node.path, task.name);
            // A cache rule only makes reuse stricter, never overriding the block's own veto.
            if node.op.attributes.cache == Some(wir::Cache::Never) {
                task.reuse = Reuse::Disabled;
            }
        }
        if !child.busy() && !child.finished() {
            self.end_child(index);
        }
        tasks
    }

    /// Finishes a child, once, and binds its results to the node's outputs.
    fn end_child(&mut self, index: usize) {
        let (at, child) = &mut self.children[index];
        let at = *at;
        let finished = child.finish();
        let node = self.progress.node(at);
        let (path, defs) = (&node.path, &self.document.types);
        let finished = match (finished, self.launched.get(&at)) {
            (Ok(Some(emitted)), Some((folder, bound))) => {
                let targets = child.targets();
                let outputs = &node.op.outputs;
                let values = nextflow::outputs(emitted, targets, outputs, bound, folder, defs);
                values
                    .map(|values| Some(Json::Object(values)))
                    .map_err(DriverError::new)
            }
            (finished, _) => finished,
        };
        let result = match finished {
            Ok(Some(Json::Object(results))) => (node.op.outputs.iter())
                .map(|port| {
                    let value = results.get(&port.name).cloned().unwrap_or_default();
                    let value = widen(value, &port.r#type, defs);
                    match wir::fits(&value, &port.r#type, defs) {
                        true => Ok((port.name.clone(), value)),
                        false => Err(format!(
                            "op {path}: output {} does not fit its type",
                            port.name
                        )),
                    }
                })
                .collect::<Result<Map<String, Json>, String>>(),
            Ok(_) => Err(format!("op {path}: the block gave no results")),
            Err(error) => Err(format!("op {path}: {}", error.message)),
        };
        match result {
            Ok(values) => self.progress.end(at, Status::Succeeded, values, None),
            Err(message) => self
                .progress
                .end(at, Status::Failed, Map::new(), Some(message)),
        }
    }

    /// Where the document source `id` is: a URL, or a path from the folder of the document.
    fn locate(&self, id: &str) -> Result<PathBuf, DriverError> {
        let source = (self.document.sources.iter())
            .find(|source| source.id == id)
            .ok_or_else(|| DriverError::new(format!("no source {id}")))?;
        Ok(match source.location.contains("://") {
            true => PathBuf::from(&source.location),
            false => self.base.join(&source.location),
        })
    }

    /// Where the job files of the native blocks of this run are written.
    fn job_folder(&mut self) -> Result<PathBuf, DriverError> {
        if self.folder.is_none() {
            let scratch = &self.launch.scratch;
            let made = match scratch.as_os_str().is_empty() {
                true => tempfile::Builder::new().prefix("wir-").tempdir(),
                false => std::fs::create_dir_all(scratch)
                    .and_then(|()| tempfile::Builder::new().prefix("wir-").tempdir_in(scratch)),
            };
            let made = made.map_err(|error| DriverError::new(format!("no job folder: {error}")))?;
            self.folder = Some(made);
        }
        Ok(self
            .folder
            .as_ref()
            .map(|folder| folder.path().to_owned())
            .unwrap_or_default())
    }

    /// The folder of the results of the run; each native block delivers to its op path in it.
    fn output_root(&self) -> PathBuf {
        match &self.launch.output_dir {
            Some(folder) if folder.to_string_lossy().contains("://") => folder.clone(),
            Some(folder) => std::path::absolute(folder).unwrap_or(folder.clone()),
            None => std::env::current_dir().unwrap_or_default(),
        }
    }

    /// Hands an engine call that names a task to the child that owns the task.
    fn owner(&self, task: TaskId) -> Result<usize, DriverError> {
        (self.owners.get(&task).copied())
            .ok_or_else(|| DriverError::new(format!("task {task} is not a task of this run")))
    }
}

impl Driver for WirDriver {
    fn language(&self) -> Language {
        Language::Wir
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let entry = (self.document.graphs.iter()).find(|graph| graph.name == self.document.entry);
        let ports = entry
            .map(|graph| graph.inputs.as_slice())
            .unwrap_or_default();
        self.progress.inputs = inputs::read(&self.launch, ports, &self.document.types)?;
        self.advance()
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        if self.tasks.contains_key(&outcome.id) {
            let mut tasks = self.complete_task(&outcome);
            tasks.extend(self.advance()?);
            return Ok(tasks);
        }
        let index = self.owner(outcome.id)?;
        let last = &mut self.last;
        let tasks = self.children[index].1.complete(outcome, &mut || {
            *last += 1;
            TaskId(*last)
        })?;
        let mut tasks = self.adopt(index, tasks);
        tasks.extend(self.advance()?);
        Ok(tasks)
    }

    fn wake_at(&self) -> Option<Instant> {
        self.children
            .iter()
            .filter_map(|(_, child)| child.wake_at())
            .min()
    }

    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        let mut tasks = Vec::new();
        for index in 0..self.children.len() {
            if self.children[index]
                .1
                .wake_at()
                .is_some_and(|time| time <= Instant::now())
            {
                let last = &mut self.last;
                let woken = self.children[index].1.wake(&mut || {
                    *last += 1;
                    TaskId(*last)
                })?;
                tasks.extend(self.adopt(index, woken));
            }
        }
        tasks.extend(self.advance()?);
        Ok(tasks)
    }

    fn cancellations(&mut self) -> Vec<TaskId> {
        (self.children.iter_mut())
            .flat_map(|(_, child)| child.cancellations())
            .collect()
    }

    /// A task op hears of failures that no machine retry can mend, so that it errors and
    /// a dependency on `errored` can hold.
    fn handles_error(&self, task: TaskId, error: &ExecuteError) -> bool {
        if self.tasks.contains_key(&task) {
            return !error.retryable();
        }
        (self.owner(task)).is_ok_and(|index| self.children[index].1.handles_error(task, error))
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        if self.tasks.contains_key(&task) {
            return self.accepted.remove(&(task, attempt));
        }
        match self.owner(task) {
            Ok(index) => self.children[index].1.take_acceptance(task, attempt),
            Err(_) => false,
        }
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        for index in 0..self.children.len() {
            if !self.children[index].1.finished() {
                self.end_child(index);
            }
        }
        let failures = &self.progress.failures;
        if let Some(first) = failures.first() {
            return Err(DriverError::new(match failures.len() {
                1 => first.clone(),
                count => format!("{first}; and {} more ops failed", count - 1),
            }));
        }
        let results = self.progress.outputs().map_err(DriverError::new)?;
        if !self.launch.simulated {
            let mut results = Json::Object(results);
            let root = self.output_root();
            deliver(&mut results, &root, &mut self.delivering);
            if self.settings.print {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&results).unwrap_or_default()
                );
            }
            self.results = Some(results);
        }
        Ok(())
    }

    fn failed(&mut self, error: &ExecuteError) -> Result<(), DriverError> {
        let mut problems = Vec::new();
        for (_, child) in &mut self.children {
            if let Err(problem) = child.failed(error) {
                problems.push(problem.message);
            }
        }
        match problems.is_empty() {
            true => Ok(()),
            false => Err(DriverError::new(problems.join("; "))),
        }
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = std::mem::take(&mut self.delivering);
        deliveries.extend((self.children.iter_mut()).flat_map(|(_, child)| child.deliveries()));
        deliveries
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        for (_, child) in &mut self.children {
            if let Some(result) = child.delivered(delivery) {
                return result;
            }
        }
        Ok(())
    }

    fn notes(&mut self) -> Vec<Note> {
        (self.children.iter_mut())
            .flat_map(|(_, child)| child.notes())
            .collect()
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.files = Some(files);
    }

    fn use_cancel(&mut self, cancel: CancellationToken) {
        self.cancel = Some(cancel);
    }

    fn results(&self) -> Option<Json> {
        self.results.clone()
    }

    /// A failure that no dependency waits for stops new ops unless the run keeps going; a
    /// block that drains stops new tasks of the whole run, as it would alone.
    fn draining(&self) -> bool {
        (!self.progress.failures.is_empty() && !self.launch.keep_going)
            || self.children.iter().any(|(_, child)| child.draining())
    }
}

/// Delivers the files and folders of a result value that are not in the output folder yet,
/// like those of task ops, to `root` by their basenames, and names them there.
fn deliver(value: &mut Json, root: &Path, deliveries: &mut Vec<Delivery>) {
    match value {
        Json::Array(items) => items
            .iter_mut()
            .for_each(|item| deliver(item, root, deliveries)),
        Json::Object(object) => {
            let class = object.get("class").and_then(Json::as_str);
            let location = object
                .get("location")
                .and_then(Json::as_str)
                .map(str::to_owned);
            if matches!(class, Some("File" | "Directory"))
                && let Some(location) = location
            {
                let local =
                    cwl_parser::iri::file_path(&location).unwrap_or(PathBuf::from(&location));
                let name = object.get("basename").and_then(Json::as_str);
                let name = name
                    .or_else(|| location.rsplit('/').next())
                    .unwrap_or("output");
                if !local.starts_with(root) {
                    let target = root.join(name).to_string_lossy().into_owned();
                    deliveries.push(Delivery {
                        location,
                        target: target.clone(),
                        mode: DeliveryMode::Copy,
                        overwrite: true,
                    });
                    object.insert("location".into(), Json::String(target));
                }
            }
            object
                .values_mut()
                .for_each(|item| deliver(item, root, deliveries));
        }
        _ => {}
    }
}
