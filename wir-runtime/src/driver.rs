//! The driver of a wir document: it starts each op of the entry graph once its inputs are
//! bound and its dependencies hold, and binds the outputs of the ops that succeed.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value as Json, json};
use wir::{Document, Effect, Kind, Language as Dialect, Native, Status};
use workflow_graph::{
    CancellationToken, Delivery, DeliveryMode, Driver, DriverError, ExecuteError, Files, Language,
    Launch, Note, Outcome, Reuse, Task, TaskId,
};

use crate::adapter::{self, Adapters, Changes, Ending, TaskCall};
use crate::argo;
use crate::callback::Callbacks;
use crate::child::Child;
use crate::cwl;
use crate::engine;
use crate::inputs;
use crate::looping;
use crate::nextflow;
use crate::operator;
use crate::plan::{Node, plan};
use crate::processes;
use crate::regions;
use crate::snakemake;
use crate::state::{Progress, State, widen};
use crate::task;
use crate::wdl;

/// How a run makes the drivers of native blocks, and what it shows.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The services of Argo blocks, like the S3 providers of the config file.
    pub argo: argo_runtime::Services,
    /// The options of CWL blocks. A block never prints its output object, and the launch
    /// says whether it is simulated.
    pub cwl: cwl_runtime::Options,
    /// How the drivers of Nextflow blocks get their settings, with a launch folder each.
    pub nextflow: nextflow::Options,
    /// What the drivers of Snakemake blocks use; each starts in a folder of its own.
    pub snakemake: snakemake::Options,
    /// The options of WDL blocks. A block never prints its outputs.
    pub wdl: wdl_runtime::Options,
    /// Whether a real run prints the outputs of its graph, as JSON.
    pub print: bool,
    /// How the run fetches the repository of a source with a `git:` digest.
    pub repositories: Repositories,
    /// Where `view` ops print.
    pub print_sink: Printer,
    /// The adapters that run dialect tasks, by builder name.
    pub adapters: Adapters,
}

/// A function of the host that takes printed text.
type Printing = dyn Fn(&str) + Send + Sync;

/// Where the ops of a run print: a function of the host, else standard output.
#[derive(Clone, Default)]
pub struct Printer(Option<Arc<Printing>>);

impl Printer {
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static) -> Printer {
        Printer(Some(Arc::new(sink)))
    }

    pub fn print(&self, text: &str) {
        match &self.0 {
            Some(sink) => sink(text),
            None => print!("{text}"),
        }
    }
}

impl std::fmt::Debug for Printer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Printer { .. }")
    }
}

/// Fetches a repository at a commit and gives the folder of its checkout.
type Fetching = dyn Fn(&str, &str) -> Result<PathBuf, DriverError> + Send + Sync;

/// How a run fetches the repository at the address of a source whose digest names a commit,
/// like `git:<commit>`. By default it cannot.
#[derive(Clone, Default)]
pub struct Repositories {
    fetch: Option<Arc<Fetching>>,
}

impl Repositories {
    pub fn new(
        fetch: impl Fn(&str, &str) -> Result<PathBuf, DriverError> + Send + Sync + 'static,
    ) -> Repositories {
        Repositories {
            fetch: Some(Arc::new(fetch)),
        }
    }

    fn fetch(&self, location: &str, commit: &str) -> Result<PathBuf, DriverError> {
        match &self.fetch {
            Some(fetch) => fetch(location, commit),
            None => Err(DriverError::new(format!(
                "{location}: this run cannot fetch repositories"
            ))),
        }
    }
}

impl std::fmt::Debug for Repositories {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Repositories { .. }")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            argo: argo_runtime::Services::default(),
            cwl: cwl_runtime::Options::default(),
            nextflow: nextflow::Options::default(),
            snakemake: snakemake::Options::default(),
            wdl: wdl_runtime::Options::default(),
            print: true,
            repositories: Repositories::default(),
            print_sink: Printer::default(),
            adapters: Adapters::default(),
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
    /// The adapters of dialect tasks that started, with their tasks.
    dialects: adapter::Dialects,
    /// Counts the calls of the engine, so that a planner hears of an instance after the call
    /// it ended in, once its files were delivered.
    turn: u64,
    /// The launch folder and the input values of each Nextflow block, by node.
    launched: HashMap<usize, (PathBuf, Map<String, Json>)>,
    /// The tasks of task ops, by id, with the node of each.
    tasks: HashMap<TaskId, (usize, Task, Map<String, Json>)>,
    /// The attempts of task ops that their captures accepted.
    accepted: HashSet<(TaskId, u32)>,
    /// The tasks of cancelled ops whose attempts were asked to stop.
    stopping: HashSet<TaskId>,
    /// The callbacks of task ops, like their acceptance checks.
    callbacks: Callbacks,
    /// The patch of the attempt of each task op that runs.
    patches: HashMap<TaskId, wir::TaskPatch>,
    last: u64,
    files: Option<Arc<dyn Files>>,
    cancel: Option<CancellationToken>,
    /// Where the job files of native blocks are written.
    folder: Option<tempfile::TempDir>,
    results: Option<Json>,
    /// The files of the graph outputs that task ops made, to deliver to the output folder.
    delivering: Vec<Delivery>,
    /// A native block ended in this call. Its results name the files it delivers, which the
    /// engine delivers after the call, so the ops after it start on the next wake.
    ended: bool,
    /// The stream ops that run, by node.
    operators: operator::Running,
    /// Stream ops have steps left to take on the next wake.
    stepping: bool,
    /// The processes that run, by node.
    processes: processes::Processes,
    /// The loops that run, by node.
    loops: looping::Loops,
    /// The dynamic regions that run, by node.
    regions: regions::Regions,
    /// The text of each source that a scope names, by source id.
    scopes: HashMap<String, String>,
    /// The file area of the run, made when an op of the document writes files.
    file_area: PathBuf,
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
        let target = std::path::absolute(&launch.target).unwrap_or(launch.target.clone());
        let base = target.parent().unwrap_or(Path::new("/")).to_owned();
        let mut document = document;
        let ports = (document.graphs.iter_mut()).flat_map(|graph| {
            let inner = graph.ops.iter_mut().flat_map(|op| op.inputs.iter_mut());
            graph.inputs.iter_mut().chain(inner)
        });
        for default in ports.filter_map(|port| port.default.as_mut()) {
            inputs::absolute(default, &base);
        }
        let progress = Progress::new(plan(&document, &settings.adapters)?, document.types.clone());
        WirDriver {
            launch: launch.clone(),
            settings,
            base,
            document,
            progress,
            children: Vec::new(),
            owners: HashMap::new(),
            dialects: adapter::Dialects::default(),
            turn: 0,
            launched: HashMap::new(),
            tasks: HashMap::new(),
            accepted: HashSet::new(),
            stopping: HashSet::new(),
            callbacks: Callbacks::default(),
            patches: HashMap::new(),
            last: 0,
            files: None,
            cancel: None,
            folder: None,
            results: None,
            delivering: Vec::new(),
            ended: false,
            operators: operator::Running::new(),
            stepping: false,
            processes: processes::Processes::new(),
            loops: looping::Loops::new(),
            regions: regions::Regions::new(),
            scopes: HashMap::new(),
            file_area: PathBuf::new(),
        }
        .checked()
    }

    fn checked(mut self) -> Result<Self, DriverError> {
        self.scopes = self.read_scopes()?;
        let ops = self.document.graphs.iter().flat_map(|graph| &graph.ops);
        let writes = |op: &wir::Op| match &op.kind {
            Kind::Split(split) => split.files.is_some(),
            Kind::CollectFile(_) => true,
            _ => false,
        };
        if ops.into_iter().any(writes) {
            self.file_area = self.job_folder()?.join("files");
        }
        Ok(self)
    }

    /// The values of the outputs of the entry graph once the run ended, also of a simulated
    /// run, whose results stay unset.
    pub fn outputs(&self) -> Option<Json> {
        let entry = (self.document.graphs.iter()).find(|graph| graph.name == self.document.entry);
        let ports = entry.map_or(&[][..], |graph| &graph.outputs);
        self.progress.outputs(ports).ok().map(Json::Object)
    }

    /// Starts every node that can start, until a native block ends; the tasks they ask for.
    /// Steps of stream ops run in passes; after this many, the driver wakes again later.
    fn advance(&mut self) -> Result<Vec<Task>, DriverError> {
        const PASSES: usize = 64;
        self.turn += 1;
        let mut tasks = Vec::new();
        let mut passes = 0;
        self.stepping = false;
        while !self.ended {
            let draining = self.draining();
            if let Some((at, bound)) = self.progress.next(draining) {
                tasks.extend(self.begin(at, bound)?);
                continue;
            }
            if passes == PASSES {
                self.stepping = !self.operators.is_empty()
                    || !self.processes.is_empty()
                    || !self.loops.is_empty()
                    || !self.regions.is_empty();
                break;
            }
            passes += 1;
            let context = engine::Context {
                document: &self.document,
                base: &self.base,
                javascript: &self.settings.cwl.javascript,
                scopes: &self.scopes,
                files: &self.file_area,
                printer: &self.settings.print_sink,
                simulated: self.launch.simulated,
            };
            let stepped = operator::step_all(&mut self.progress, &mut self.operators, &context);
            let rounds =
                processes::step_all(&mut self.progress, &self.document, &mut self.processes);
            let runs = looping::step_all(&mut self.progress, &self.document, &mut self.loops);
            let plans = self.step_regions();
            if !stepped && !rounds && !runs && !plans {
                break;
            }
        }
        // A planner hears of ended instances on a later wake.
        let progress = &self.progress;
        let unheard =
            |region: &regions::Region| region.spec.planner.is_some() && region.unheard(progress);
        self.stepping |= self.regions.values().any(unheard);
        Ok(tasks)
    }

    /// Starts a node with its input values; the tasks it asks for.
    fn begin(&mut self, at: usize, bound: Map<String, Json>) -> Result<Vec<Task>, DriverError> {
        if let Kind::Dynamic(spec) = &self.progress.node(at).op.kind {
            self.regions
                .insert(at, regions::Region::new(spec.clone(), bound));
            return Ok(Vec::new());
        }
        if let Kind::Loop(spec) = &self.progress.node(at).op.kind {
            self.loops
                .insert(at, looping::Looping::new(spec.clone(), bound));
            return Ok(Vec::new());
        }
        if let Kind::Process(spec) = &self.progress.node(at).op.kind {
            let rounding = processes::Rounding::new(spec.clone(), self.progress.node(at));
            self.processes.insert(at, rounding);
            return Ok(Vec::new());
        }
        let defs = &self.document.types;
        if let Some(stepped) = operator::make_typed(self.progress.node(at), defs) {
            self.operators.insert(at, stepped);
            return Ok(Vec::new());
        }
        let context = engine::Context {
            document: &self.document,
            base: &self.base,
            javascript: &self.settings.cwl.javascript,
            scopes: &self.scopes,
            files: &self.file_area,
            printer: &self.settings.print_sink,
            simulated: self.launch.simulated,
        };
        if engine::begin(&mut self.progress, at, &bound, &context)? {
            return Ok(Vec::new());
        }
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
            Kind::Task(wir::Task {
                builder: Some(builder),
                ..
            }) => self.start_dialect(at, &builder, &bound),
            Kind::Task(_) => {
                self.last += 1;
                match self.attempt_task(at, TaskId(self.last), &bound, (1, Json::Null)) {
                    Ok(task) => {
                        self.tasks.insert(task.id, (at, task.clone(), bound));
                        Ok(vec![task])
                    }
                    Err(why) => {
                        let message = format!("op {}: {why}", self.progress.node(at).path);
                        (self.progress).end(at, Status::Errored, Map::new(), Some(message));
                        Ok(Vec::new())
                    }
                }
            }
            _ => {
                let path = &self.progress.node(at).path;
                Err(DriverError::unsupported(format!(
                    "op {path}: uwe does not run this op kind yet"
                )))
            }
        }
    }

    /// The text of the source of a scope, once its byte range is checked against it.
    fn scope_text(&self, scope: &wir::Scope) -> Result<String, DriverError> {
        let path = self.locate(&scope.source)?;
        let text = std::fs::read_to_string(&path).map_err(|error| {
            DriverError::new(format!("cannot read {}: {error}", path.display()))
        })?;
        let (start, end) = (scope.start as usize, scope.end as usize);
        if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            let message = format!(
                "the scope {start}..{end} is no range of characters of {}",
                scope.source
            );
            return Err(DriverError::unsupported(message));
        }
        Ok(text)
    }

    /// Reads the source of every scope of the document and checks each scope against it,
    /// before anything runs.
    fn read_scopes(&self) -> Result<HashMap<String, String>, DriverError> {
        let mut scopes = Vec::new();
        for op in self.document.graphs.iter().flat_map(|graph| &graph.ops) {
            scopes_in(
                &serde_json::to_value(&op.kind).unwrap_or_default(),
                &mut scopes,
            );
        }
        let mut texts = HashMap::new();
        for scope in scopes {
            let text = self.scope_text(&scope)?;
            texts.insert(scope.source, text);
        }
        Ok(texts)
    }

    /// The task of an attempt of a task op, from 1, built from the base settings and the patch
    /// that its `directives` give for the attempt, with how the attempt before was rejected.
    fn attempt_task(
        &mut self,
        at: usize,
        id: TaskId,
        bound: &Map<String, Json>,
        (attempt, previous): (u32, Json),
    ) -> Result<Task, String> {
        let node = self.progress.node(at);
        let Kind::Task(spec) = &node.op.kind else {
            return Err("the op is no task".into());
        };
        let directives = spec
            .retry
            .as_ref()
            .and_then(|retry| retry.directives.as_ref());
        let patch = match directives {
            None => wir::TaskPatch::default(),
            Some(directives) => {
                let index = adapter::frame(&self.progress, at).rounds.last().copied();
                let argument = task::attempt_context(bound, index.unwrap_or(0), attempt, previous);
                let context = engine::Context {
                    document: &self.document,
                    base: &self.base,
                    javascript: &self.settings.cwl.javascript,
                    scopes: &self.scopes,
                    files: &self.file_area,
                    printer: &self.settings.print_sink,
                    simulated: self.launch.simulated,
                };
                let callbacks = &mut self.callbacks;
                let given = task::callback(callbacks, &context, directives, argument, bound)
                    .map_err(|why| format!("the retry directives: {why}"))?;
                serde_json::from_value(given)
                    .map_err(|error| format!("the retry directives give no task patch: {error}"))?
            }
        };
        let (spec, op) = task::patched(spec, &node.op, &patch);
        let mut task = task::build(id, (&node.path, &node.step), &op, &spec, bound);
        task.site = op.attributes.place.clone();
        self.patches.insert(id, patch);
        Ok(task)
    }

    /// The next attempt of a rejected task, when the retry limit and `retry_if` allow it, or
    /// why its retry rules failed.
    fn retry_task(
        &mut self,
        at: usize,
        outcome: &Outcome,
        previous: Json,
    ) -> Result<Option<Task>, String> {
        let node = self.progress.node(at);
        let Kind::Task(spec) = &node.op.kind else {
            return Ok(None);
        };
        let retry = spec.retry.as_ref();
        let limit = (node.op.attributes.retry).unwrap_or(retry.map_or(0, |retry| retry.limit));
        let cancelled = outcome.error == Some(workflow_graph::TaskError::Cancelled);
        if cancelled || outcome.attempt > limit {
            return Ok(None);
        }
        let bound = self.tasks[&outcome.id].2.clone();
        let attempt = outcome.attempt + 1;
        if let Some(test) = retry.and_then(|retry| retry.retry_if.as_ref()) {
            let index = adapter::frame(&self.progress, at).rounds.last().copied();
            let argument =
                task::attempt_context(&bound, index.unwrap_or(0), attempt, previous.clone());
            let context = engine::Context {
                document: &self.document,
                base: &self.base,
                javascript: &self.settings.cwl.javascript,
                scopes: &self.scopes,
                files: &self.file_area,
                printer: &self.settings.print_sink,
                simulated: self.launch.simulated,
            };
            match task::callback(&mut self.callbacks, &context, test, argument, &bound) {
                Ok(Json::Bool(true)) => {}
                Ok(Json::Bool(false)) => return Ok(None),
                Ok(other) => return Err(format!("retry_if gave {other}, not a bool")),
                Err(why) => return Err(format!("retry_if: {why}")),
            }
        }
        let task = self.attempt_task(at, outcome.id, &bound, (attempt, previous))?;
        if let Some(entry) = self.tasks.get_mut(&outcome.id) {
            entry.1 = task.clone();
        }
        Ok(Some(task))
    }

    /// Reads the outputs of a task op from its outcome. An attempt that is not accepted runs
    /// again while the op's `retry` allows it; then the op fails, or errors when the task
    /// could not run.
    fn complete_task(&mut self, outcome: &Outcome) -> Vec<Task> {
        let (at, _, bound) = &self.tasks[&outcome.id];
        let (at, node) = (*at, self.progress.node(*at));
        // The op of a stopped attempt was cancelled already.
        if matches!(self.progress.states[at], State::Ended(..)) {
            return Vec::new();
        }
        let Kind::Task(spec) = &node.op.kind else {
            return Vec::new();
        };
        let files = self.files.as_deref();
        let ended = task::Ended {
            outcome,
            files,
            bound,
        };
        // An attempt is judged by its own patch, like its success codes.
        let patch = self.patches.get(&outcome.id).cloned().unwrap_or_default();
        let (spec, _) = task::patched(spec, &node.op, &patch);
        let spec = &spec;
        let captured = task::capture(spec, &node.op.outputs, ended, &self.document.types);
        let context = engine::Context {
            document: &self.document,
            base: &self.base,
            javascript: &self.settings.cwl.javascript,
            scopes: &self.scopes,
            files: &self.file_area,
            printer: &self.settings.print_sink,
            simulated: self.launch.simulated,
        };
        let index = adapter::frame(&self.progress, at).rounds.last().copied();
        let accepted = captured.and_then(|(values, empty)| match &spec.accept {
            None => Ok((values, empty)),
            Some(accept) => {
                let attempt = (bound, &values, index.unwrap_or(0));
                let callbacks = &mut self.callbacks;
                match task::accepts(callbacks, &context, accept, attempt, outcome) {
                    Ok(true) => Ok((values, empty)),
                    Ok(false) => Err("the acceptance check does not hold".to_owned()),
                    Err(why) => Err(format!("the acceptance check: {why}")),
                }
            }
        });
        match accepted {
            Ok((values, empty)) => {
                self.accepted.insert((outcome.id, outcome.attempt));
                (self.progress).end_with(at, Status::Succeeded, values, empty);
                Vec::new()
            }
            Err(why) => {
                // A task that could not run says why in the last line of its error output.
                let said = outcome.stderr.trim().lines().last();
                let (status, mut why) = match (&outcome.error, said) {
                    (Some(_), Some(said)) => (Status::Errored, said.to_owned()),
                    (Some(_), None) => (Status::Errored, why),
                    (None, _) => (Status::Failed, why),
                };
                let ignore = (spec.retry.as_ref())
                    .is_some_and(|retry| retry.failure == wir::FailureRule::Ignore);
                let previous = json!({"status": status, "exit_code": outcome.exit_code,
                    "capture_error": why});
                match self.retry_task(at, outcome, previous) {
                    Ok(Some(task)) => return vec![task],
                    Ok(None) if ignore => {
                        let node = self.progress.node(at);
                        let empty = node.op.outputs.iter().map(|port| port.name.clone());
                        let empty = empty.collect();
                        (self.progress).end_with(at, Status::Succeeded, Map::new(), empty);
                        return Vec::new();
                    }
                    Ok(None) => {}
                    Err(problem) => why = problem,
                }
                let node = self.progress.node(at);
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
            // A Snakemake config file and an Argo parameter file take the values of params.
            Dialect::Snakemake => (folder.join("config.json"), nextflow::params(&bound)),
            Dialect::Argo => (folder.join("parameters.json"), nextflow::params(&bound)),
            Dialect::Wdl => {
                let ports = &self.progress.node(at).op.inputs;
                let values = wdl::pairs(bound.clone(), ports, &self.document.types);
                (folder.join("inputs.json"), wdl::job(values))
            }
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
            Dialect::Argo if !launch.profiles.is_empty() => {
                let message = format!("op {path}: Argo blocks take no profiles");
                return Err(DriverError::unsupported(message));
            }
            Dialect::Argo => argo::block(&launch, &self.settings.argo)?,
            Dialect::Wdl if !launch.profiles.is_empty() || !launch.configs.is_empty() => {
                let message = format!("op {path}: WDL blocks take no profiles or configs");
                return Err(DriverError::unsupported(message));
            }
            Dialect::Wdl => wdl::block(&launch, &self.settings.wdl)?,
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
            mark(task, node, true);
        }
        if !child.busy() && !child.finished() {
            self.end_child(index);
        }
        tasks
    }

    /// Finishes a child, once, and binds its results to the node's outputs.
    fn end_child(&mut self, index: usize) {
        self.ended = true;
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
        // A simulated CWL block reads made-up files, which can leave an output null.
        let lenient = self.launch.simulated
            && matches!(&node.op.kind, Kind::Native(native) if native.language == Dialect::Cwl);
        let result = match finished {
            Ok(Some(Json::Object(results))) => typed(&results, &node.op.outputs, defs, lenient)
                .map_err(|port| format!("op {path}: output {port} does not fit its type")),
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

    /// Starts a dialect task op through the adapter of its source context. An adapter that
    /// cannot start the op errors it.
    fn start_dialect(
        &mut self,
        at: usize,
        builder: &wir::TaskBuilder,
        bound: &Map<String, Json>,
    ) -> Result<Vec<Task>, DriverError> {
        let index = self.open_dialect(builder);
        let node = self.progress.node(at);
        let call = TaskCall {
            call: adapter::CallId(at),
            frame: adapter::frame(&self.progress, at),
            builder,
            op: &node.op,
            inputs: bound,
        };
        let last = &mut self.last;
        let started = index.and_then(|index| {
            let next = &mut || {
                *last += 1;
                TaskId(*last)
            };
            self.dialects.start(index, &call, next)
        });
        match started {
            Ok(changes) => Ok(self.take_changes(changes)),
            Err(error) if error.unsupported => Err(error),
            Err(error) => {
                let message = format!("op {}: {}", self.progress.node(at).path, error.message);
                (self.progress).end(at, Status::Errored, Map::new(), Some(message));
                Ok(Vec::new())
            }
        }
    }

    /// The adapter of the source context of a builder, opened the first time.
    fn open_dialect(&mut self, builder: &wir::TaskBuilder) -> Result<usize, DriverError> {
        let task = builder.task();
        let name = adapter::builder_name(builder);
        let key = (
            name,
            task.source.clone(),
            task.profiles.clone(),
            task.configs.clone(),
        );
        if let Some(index) = self.dialects.find(&key) {
            return Ok(index);
        }
        let folder = self.job_folder()?.join(format!("{name}-{}", task.source));
        let configs = task.configs.iter().map(|id| self.locate(id));
        let opening = adapter::Opening {
            builder,
            sources: &self.document.sources,
            source: self.locate(&task.source)?,
            configs: configs.collect::<Result<_, _>>()?,
            inputs: &self.progress.inputs,
            launch: &self.launch,
            folder,
            output: self.output_root(),
            files: self.files.clone(),
            cancel: self.cancel.clone(),
        };
        let opened = self.settings.adapters.open(&opening)?;
        Ok(self.dialects.add(key, opened))
    }

    /// Ends the op instances that an adapter ended; the tasks it asks for, under their op.
    fn take_changes(&mut self, changes: Changes) -> Vec<Task> {
        let mut tasks = Vec::with_capacity(changes.tasks.len());
        let ended = |progress: &Progress, call: adapter::CallId| {
            matches!(progress.states[call.0], State::Ended(..))
        };
        let changes = Changes {
            tasks: (changes.tasks.into_iter())
                .filter(|(call, _)| !ended(&self.progress, *call))
                .collect(),
            ended: (changes.ended.into_iter())
                .filter(|(call, _)| !ended(&self.progress, *call))
                .collect(),
            ready: changes.ready,
        };
        // A ready daemon has its control result before it ends.
        (self.progress.daemons).extend(changes.ready.iter().map(|call| call.0));
        for (call, mut task) in changes.tasks {
            let prefixed = self.dialects.prefixed(task.id);
            mark(&mut task, self.progress.node(call.0), prefixed);
            tasks.push(task);
        }
        for (call, ending) in changes.ended {
            let node = self.progress.node(call.0);
            let (path, at) = (node.path.clone(), call.0);
            let failed = |why| Some(format!("op {path}: {why}"));
            match ending {
                Ending::Succeeded { values, empty } => {
                    let ports: Vec<wir::Port> = (node.op.outputs.iter())
                        .filter(|port| !empty.contains(&port.name))
                        .cloned()
                        .collect();
                    match typed(&values, &ports, &self.document.types, false) {
                        Ok(values) => {
                            (self.progress).end_with(at, Status::Succeeded, values, empty);
                        }
                        Err(port) => {
                            let why = failed(format!("output {port} does not fit its type"));
                            self.progress.end(at, Status::Failed, Map::new(), why);
                        }
                    }
                }
                Ending::Skipped => {
                    let empty = node.op.outputs.iter().map(|port| port.name.clone());
                    let empty = empty.collect();
                    (self.progress).end_with(at, Status::Skipped, Map::new(), empty);
                }
                Ending::Ignored(_) => {
                    let empty = node.op.outputs.iter().map(|port| port.name.clone());
                    let empty = empty.collect();
                    (self.progress).end_with(at, Status::Succeeded, Map::new(), empty);
                }
                Ending::Failed(why) => {
                    self.progress
                        .end(at, Status::Failed, Map::new(), failed(why))
                }
                Ending::Errored(why) => {
                    (self.progress).end(at, Status::Errored, Map::new(), failed(why));
                }
            }
        }
        tasks
    }

    /// Takes in the plan updates of every region, and ends the regions that settled; whether
    /// any moved. A region whose update cannot be adopted fails.
    fn step_regions(&mut self) -> bool {
        let mut moved = false;
        for at in self.regions.keys().copied().collect::<Vec<usize>>() {
            match self.step_region(at) {
                Ok(step) => moved |= step,
                Err(why) => {
                    regions::fail(&mut self.progress, &mut self.regions, at, why);
                    moved = true;
                }
            }
        }
        moved | regions::settle(&mut self.progress, &mut self.regions)
    }

    fn step_region(&mut self, at: usize) -> Result<bool, String> {
        let Some(mut region) = self.regions.remove(&at) else {
            return Ok(false);
        };
        let stepped = self.plan_region(&mut region, at);
        self.regions.insert(at, region);
        stepped
    }

    /// Adopts the plans of a region: those of its plan input, or those its planner gives
    /// first and after each instance ended, once no delivery of the run waits.
    fn plan_region(&mut self, region: &mut regions::Region, at: usize) -> Result<bool, String> {
        let mut updates = region.given(&mut self.progress, at)?;
        if let Some(planner) = region.spec.planner.clone() {
            let builder = adapter::planner_builder(&planner);
            let index = self.open_dialect(&builder).map_err(|error| error.message)?;
            let to_json = |update| serde_json::to_value(update).unwrap_or_default();
            if !std::mem::replace(&mut region.planned, true) {
                let call = adapter::RegionCall {
                    call: adapter::CallId(at),
                    frame: adapter::frame(&self.progress, at),
                    planner: &planner,
                    inputs: &region.bound,
                };
                let update = self.dialects.plan(index, &call);
                updates.push(to_json(update.map_err(|error| error.message)?));
            }
            let delivering = !self.delivering.is_empty() || self.dialects.delivering();
            for ended in region.ended(&self.progress, self.turn) {
                if delivering {
                    break;
                }
                let replanned =
                    self.dialects
                        .replan(index, adapter::CallId(at), (&ended.0, ended.1));
                updates.extend(replanned.map_err(|error| error.message)?.map(to_json));
            }
        }
        let moved = !updates.is_empty();
        let checks = (&self.document, &self.settings.adapters);
        for update in updates {
            region.adopt(&mut self.progress, checks, at, update)?;
        }
        Ok(moved)
    }

    /// Where the document source `id` is: the checkout of a repository at the commit of a
    /// `git:` digest, a URL, or a path from the folder of the document.
    fn locate(&self, id: &str) -> Result<PathBuf, DriverError> {
        let source = (self.document.sources.iter())
            .find(|source| source.id == id)
            .ok_or_else(|| DriverError::new(format!("no source {id}")))?;
        let commit = (source.digest.as_deref()).and_then(|digest| digest.strip_prefix("git:"));
        Ok(match (source.location.contains("://"), commit) {
            (true, Some(commit)) => self.settings.repositories.fetch(&source.location, commit)?,
            (true, None) => PathBuf::from(&source.location),
            (false, _) => self.base.join(&source.location),
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
        // A stream input is given as the array of its items.
        let given: Vec<wir::Port> = (ports.iter())
            .map(|port| match port.flow.is_value() {
                true => port.clone(),
                false => wir::Port {
                    r#type: wir::Type::Array(wir::ArrayType {
                        items: Box::new(port.r#type.clone()),
                        nonempty: false,
                    }),
                    flow: wir::Flow::Value,
                    ..port.clone()
                },
            })
            .collect();
        let mut values = inputs::read(&self.launch, &given, &self.document.types)?;
        let streams: Vec<String> = (ports.iter())
            .filter(|port| !port.flow.is_value())
            .map(|port| port.name.clone())
            .collect();
        self.progress.feed(&values, &streams)?;
        values.retain(|name, _| !streams.contains(name));
        self.progress.inputs = values;
        self.progress.activate(None)?;
        self.advance()
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        if self.tasks.contains_key(&outcome.id) {
            let mut tasks = self.complete_task(&outcome);
            tasks.extend(self.advance()?);
            return Ok(tasks);
        }
        if self.dialects.owns(outcome.id) {
            let last = &mut self.last;
            let changes = self.dialects.complete(outcome, &mut || {
                *last += 1;
                TaskId(*last)
            })?;
            let mut tasks = self.take_changes(changes);
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
        if self.ended || self.stepping {
            return Some(Instant::now());
        }
        let children = self
            .children
            .iter()
            .filter_map(|(_, child)| child.wake_at());
        children.chain(self.dialects.wake_at()).min()
    }

    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        self.ended = false;
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
        let last = &mut self.last;
        let changes = self.dialects.wake(&mut || {
            *last += 1;
            TaskId(*last)
        })?;
        tasks.extend(self.take_changes(changes));
        tasks.extend(self.advance()?);
        Ok(tasks)
    }

    fn cancellations(&mut self) -> Vec<TaskId> {
        let mut tasks: Vec<TaskId> = (self.children.iter_mut())
            .flat_map(|(_, child)| child.cancellations())
            .collect();
        tasks.extend(self.dialects.cancellations());
        // The tasks of ops that a failed process cancelled stop too.
        let cancelled =
            |at: usize| matches!(self.progress.states[at], State::Ended(Status::Cancelled, _));
        let ops = (self.tasks.iter()).map(|(id, (at, ..))| (*id, *at));
        for (id, at) in ops.chain(self.dialects.tasks()) {
            if cancelled(at) && self.stopping.insert(id) {
                tasks.push(id);
            }
        }
        tasks
    }

    /// A task op hears of failures that no machine retry can mend, so that it errors and
    /// a dependency on `errored` can hold.
    fn handles_error(&self, task: TaskId, error: &ExecuteError) -> bool {
        if self.tasks.contains_key(&task) {
            return !error.retryable();
        }
        if self.dialects.owns(task) {
            return self.dialects.handles_error(task, error);
        }
        (self.owner(task)).is_ok_and(|index| self.children[index].1.handles_error(task, error))
    }

    fn started(&mut self, task: TaskId, attempt: u32) {
        self.dialects.started(task, attempt);
        if let Ok(index) = self.owner(task) {
            self.children[index].1.started(task, attempt);
        }
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        if self.tasks.contains_key(&task) {
            return self.accepted.remove(&(task, attempt));
        }
        if self.dialects.owns(task) {
            return self.dialects.take_acceptance(task, attempt);
        }
        match self.owner(task) {
            Ok(index) => self.children[index].1.take_acceptance(task, attempt),
            Err(_) => false,
        }
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        operator::cancel_all(&mut self.progress, &mut self.operators);
        for at in std::mem::take(&mut self.processes).into_keys() {
            let path = self.progress.node(at).path.clone();
            self.progress.end(at, Status::Cancelled, Map::new(), None);
            self.progress
                .failures
                .push(format!("op {path}: its rounds never ended"));
        }
        for at in self.regions.keys().copied().collect::<Vec<usize>>() {
            let why = "its plan never settled".to_owned();
            regions::fail(&mut self.progress, &mut self.regions, at, why);
        }
        for at in std::mem::take(&mut self.loops).into_keys() {
            let path = self.progress.node(at).path.clone();
            self.progress.end(at, Status::Cancelled, Map::new(), None);
            let failures = &mut self.progress.failures;
            failures.push(format!("op {path}: its runs never ended"));
        }
        for at in self.dialects.open_calls() {
            if matches!(self.progress.states[at], State::Ended(..)) {
                continue;
            }
            let path = self.progress.node(at).path.clone();
            self.progress.end(at, Status::Cancelled, Map::new(), None);
            let failures = &mut self.progress.failures;
            failures.push(format!("op {path}: its task never ended"));
        }
        let failure = self.progress.failures.first().cloned();
        if let Err(error) = self.dialects.end(failure.as_deref()) {
            self.progress.failures.push(error.message);
        }
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
        let entry = (self.document.graphs.iter()).find(|graph| graph.name == self.document.entry);
        let ports = entry.map_or(&[][..], |graph| &graph.outputs);
        let results = self.progress.outputs(ports).map_err(DriverError::new)?;
        if !self.launch.simulated {
            let mut results = Json::Object(results);
            let root = self.output_root();
            let mut planned = Vec::new();
            let mut places = BTreeMap::new();
            for target in (self.children.iter()).flat_map(|(_, child)| child.targets()) {
                let place = cwl_parser::iri::file_path(target).unwrap_or(PathBuf::from(target));
                places.insert(place.clone(), place);
            }
            let sources = &self.document.sources;
            let wdl = !sources.is_empty() && sources.iter().all(|s| s.language == Dialect::Wdl);
            match (wdl, &mut results) {
                (true, Json::Object(outputs)) => {
                    for (name, value) in outputs.iter_mut() {
                        let folder = root.join(name);
                        wdl_layout(value, (&root, &folder), &mut planned, &mut places)
                            .map_err(DriverError::new)?;
                    }
                }
                _ => deliver(&mut results, &root, &mut planned, &mut places)
                    .map_err(DriverError::new)?,
            }
            nested(&planned, &places).map_err(DriverError::new)?;
            self.delivering.extend(planned);
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
        if let Err(problem) = self.dialects.end(Some(&error.to_string())) {
            problems.push(problem.message);
        }
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
        deliveries.extend(self.dialects.deliveries());
        deliveries
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        if let Some(result) = self.dialects.delivered(delivery) {
            return result;
        }
        for (_, child) in &mut self.children {
            if let Some(result) = child.delivered(delivery) {
                return result;
            }
        }
        Ok(())
    }

    fn notes(&mut self) -> Vec<Note> {
        let mut notes: Vec<Note> = (self.children.iter_mut())
            .flat_map(|(_, child)| child.notes())
            .collect();
        notes.extend(self.dialects.notes());
        notes
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

/// Names a task under its op path when `prefixed`, and gives it the cache rule and place of
/// its op. A cache rule only makes reuse stricter, never overriding the native veto.
fn mark(task: &mut Task, node: &Node, prefixed: bool) {
    // A native task publishes as its language does, also for the tasks it reuses.
    let effects = &node.op.effects;
    let lineage = node.op.attributes.cache == Some(wir::Cache::Resume)
        || effects
            .iter()
            .any(|effect| !matches!(effect, Effect::Publish(_)));
    if prefixed {
        task.name = format!("{}: {}", node.path, task.name);
    }
    match node.op.attributes.cache {
        Some(wir::Cache::Never) => task.reuse = Reuse::Disabled,
        _ if lineage && task.reuse == Reuse::Allowed => task.reuse = Reuse::Lineage,
        _ => {}
    }
    if let Some(place) = &node.op.attributes.place {
        task.site = Some(place.clone());
    }
}

/// The values of the output ports in `results`, widened to their types; else the name of a
/// port whose value does not fit. A `lenient` run gives a null output a made-up value.
fn typed(
    results: &Map<String, Json>,
    ports: &[wir::Port],
    defs: &[wir::TypeDef],
    lenient: bool,
) -> Result<Map<String, Json>, String> {
    (ports.iter())
        .map(|port| {
            let value = results.get(&port.name).cloned().unwrap_or_default();
            let value = widen(value, &port.r#type, defs);
            match wir::fits(&value, &port.r#type, defs) {
                true => Ok((port.name.clone(), value)),
                false if lenient && value.is_null() => {
                    let made = crate::expr::placeholder(&port.r#type, &port.name, defs);
                    Ok((port.name.clone(), made))
                }
                false => Err(port.name.clone()),
            }
        })
        .collect()
}

/// The scopes of the closures and expressions in the JSON form of an op kind.
fn scopes_in(value: &Json, found: &mut Vec<wir::Scope>) {
    match value {
        Json::Object(object) => {
            let scope = object
                .get("scope")
                .and_then(|scope| serde_json::from_value(scope.clone()).ok());
            found.extend(scope);
            object.values().for_each(|item| scopes_in(item, found));
        }
        Json::Array(items) => items.iter().for_each(|item| scopes_in(item, found)),
        _ => {}
    }
}

/// Delivers the files of one output of a lowered WDL document as the WDL driver does: to
/// `folder/`, or to `folder/<index>/` for many files. Files in `root` already stay there.
fn wdl_layout(
    value: &mut Json,
    (root, folder): (&Path, &Path),
    deliveries: &mut Vec<Delivery>,
    places: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), String> {
    let mut count = 0;
    each_file(value, &mut |_| count += 1);
    let mut index = 0;
    let mut result = Ok(());
    each_file(value, &mut |file| {
        let location = file
            .get("location")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let local = cwl_parser::iri::file_path(location).unwrap_or(PathBuf::from(location));
        let place = match count {
            _ if local.starts_with(root) => root.to_owned(),
            1 => folder.to_owned(),
            _ => folder.join(index.to_string()),
        };
        index += 1;
        if result.is_ok() {
            result = deliver(file, &place, deliveries, places);
        }
    });
    result
}

/// Calls `visit` with each File and Directory object of a value, in order.
fn each_file(value: &mut Json, visit: &mut impl FnMut(&mut Json)) {
    match value {
        Json::Array(items) => items.iter_mut().for_each(|item| each_file(item, visit)),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File" | "Directory") => visit(value),
            _ => (object.values_mut()).for_each(|item| each_file(item, visit)),
        },
        _ => {}
    }
}

/// Delivers the files and folders of a result value that are not in the output folder yet,
/// like those of task ops, to `root` by their basenames, and names them there. `places` holds
/// the source path of each place in `root`; a later source of a taken name gets `<name>-<n>/`.
fn deliver(
    value: &mut Json,
    root: &Path,
    deliveries: &mut Vec<Delivery>,
    places: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), String> {
    match value {
        Json::Array(items) => {
            for item in items {
                deliver(item, root, deliveries, places)?;
            }
        }
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
                let inside = local.starts_with(root);
                let mut place = match inside {
                    true => local.clone(),
                    false => root.join(name),
                };
                // As the CWL driver does, a later file of a taken name goes to a numbered folder.
                let mut number = 2;
                while !inside && places.get(&place).is_some_and(|other| *other != local) {
                    place = root.join(format!("{name}-{number}")).join(name);
                    number += 1;
                }
                if let Some(other) = places.insert(place.clone(), local.clone())
                    && other != local
                {
                    return Err(format!(
                        "two outputs go to {}; give their files different names",
                        place.display()
                    ));
                }
                if !inside {
                    let target = place.to_string_lossy().into_owned();
                    deliveries.push(Delivery {
                        location,
                        target: target.clone(),
                        mode: DeliveryMode::Copy,
                        overwrite: true,
                    });
                    object.insert("location".into(), Json::String(target));
                }
            }
            for item in object.values_mut() {
                deliver(item, root, deliveries, places)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Refuses a delivery to a place that holds, or lies in, another place of `places`: copying
/// it would replace the files below it, or the folder it lies in.
fn nested(deliveries: &[Delivery], places: &BTreeMap<PathBuf, PathBuf>) -> Result<(), String> {
    for delivery in deliveries {
        let place = Path::new(&delivery.target);
        let outer = (place.ancestors().skip(1)).find(|folder| places.contains_key(*folder));
        // Places below `place` follow it in the order of paths.
        let after = (Bound::Excluded(place), Bound::Unbounded);
        let inner = (places.range::<Path, _>(after).next())
            .map(|(path, _)| path.as_path())
            .filter(|path| path.starts_with(place));
        let pair = match (outer, inner) {
            (Some(outer), _) => Some((outer, place)),
            (None, Some(inner)) => Some((place, inner)),
            (None, None) => None,
        };
        if let Some((outer, inner)) = pair {
            return Err(format!(
                "outputs go to {} and to {} inside it; give their files different names",
                outer.display(),
                inner.display()
            ));
        }
    }
    Ok(())
}
