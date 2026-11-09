//! Dialect task ops: the adapters of language runtimes that build and judge the tasks of single
//! native task instances, a registry of them by builder name, and the adapters a run opened.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value as Json};
use wir::reserved::{Identity, PlanUpdate, Planner};
use wir::{Kind, Op, Status, TaskBuilder};
use workflow_graph::{
    CancellationToken, Delivery, DriverError, ExecuteError, Files, Launch, Note, Outcome, Task,
    TaskId,
};

use crate::state::Progress;

/// An op instance that runs through an adapter, which names it in [`Changes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(pub(crate) usize);

/// Where an op instance runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub path: String,
    /// The path without round numbers, which tasks keep as their step.
    pub step: String,
    /// The round numbers of the maps and processes around the op, outermost first; a process
    /// round gives its base round, then its choice.
    pub rounds: Vec<u64>,
}

/// A dialect task op that starts, with the values of its inputs by port.
pub struct TaskCall<'a> {
    pub call: CallId,
    pub frame: Frame,
    pub builder: &'a TaskBuilder,
    pub op: &'a Op,
    pub inputs: &'a Map<String, Json>,
}

/// A dynamic region whose planner is an adapter, with the values of its inputs by port.
pub struct RegionCall<'a> {
    pub call: CallId,
    pub frame: Frame,
    pub planner: &'a Planner,
    pub inputs: &'a Map<String, Json>,
}

/// How an op instance ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Ending {
    /// The values of its outputs by port, and the outputs that stay empty, like a missing
    /// optional output; any other output without a value is null.
    Succeeded {
        values: Map<String, Json>,
        empty: Vec<String>,
    },
    /// Its condition was false, like a Nextflow `when:`, with the values it gives some
    /// outputs anyway, like the defaults of a skipped Argo step; the others close empty.
    Skipped(Map<String, Json>),
    /// Its failure is ignored: its outputs close empty.
    Ignored(String),
    Failed(String),
    /// It could not run.
    Errored(String),
}

/// The tasks that the op instances of an adapter ask for, the instances that ended, and
/// those whose daemon is ready, like a running Argo daemon step.
#[derive(Debug, Default)]
pub struct Changes {
    pub tasks: Vec<(CallId, Task)>,
    pub ended: Vec<(CallId, Ending)>,
    pub ready: Vec<CallId>,
}

/// The runtime of a language for the dialect tasks of one source context. It keeps the native
/// state between attempts and decides acceptance and retries as the native run does.
pub trait TaskAdapter {
    /// Starts an op instance: the tasks it asks for first, or its ending.
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError>;

    /// Takes the outcome of an attempt of a task it asked for; a retry keeps the task id.
    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError>;

    fn started(&mut self, _task: TaskId, _attempt: u32) {}

    /// Whether it accepted this attempt in [`TaskAdapter::complete`]; asked once after it.
    fn take_acceptance(&mut self, _task: TaskId, _attempt: u32) -> bool {
        false
    }

    /// Whether an execution failure comes back as an outcome, for the retry rules of the
    /// language.
    fn handles_error(&self, _task: TaskId, _error: &ExecuteError) -> bool {
        false
    }

    /// When it waits for time to pass, like the backoff of a retry.
    fn wake_at(&self) -> Option<Instant> {
        None
    }

    fn wake(&mut self) -> Result<Changes, DriverError> {
        Ok(Changes::default())
    }

    /// Tasks whose attempts must stop, like those past their deadline.
    fn cancellations(&mut self) -> Vec<TaskId> {
        Vec::new()
    }

    /// The files to deliver since the last call, like those of a Nextflow `publishDir`.
    fn deliveries(&mut self) -> Vec<Delivery> {
        Vec::new()
    }

    fn delivered(&mut self, _delivery: &Delivery) -> Result<(), DriverError> {
        Ok(())
    }

    /// What it noticed since the last call, like checkpoint marks or files that workflow code
    /// read, with the task ids it gave.
    fn notes(&mut self) -> Vec<Note> {
        Vec::new()
    }

    /// The first plan of a region that it plans, like the jobs of a Snakefile.
    fn plan(&mut self, _region: &RegionCall) -> Result<PlanUpdate, DriverError> {
        Err(DriverError::unsupported("this adapter plans no regions"))
    }

    /// Hears that an instance of a region ended, after its acceptance and the delivery of
    /// its files; a new plan when it plans the region again, like after a checkpoint.
    fn replan(
        &mut self,
        _region: CallId,
        _instance: &Identity,
        _status: Status,
    ) -> Result<Option<PlanUpdate>, DriverError> {
        Ok(None)
    }

    /// Ends the source context once the run ends, with why the run failed, if it did; the
    /// files to deliver then, like the reports of a Nextflow run.
    fn end(&mut self, _failure: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        Ok(Vec::new())
    }

    /// Whether task names get the op path in front, as `<path>: <name>`.
    fn prefixed(&self) -> bool {
        true
    }
}

/// What an adapter opens with: the source context of its first op and the run around it.
pub struct Opening<'a> {
    pub builder: &'a TaskBuilder,
    /// The sources of the document, like the config values a compiler pinned.
    pub sources: &'a [wir::Source],
    /// The observers of its source and the processes they hear of, from the run hooks.
    pub observers: Vec<&'a wir::Observers>,
    pub source: PathBuf,
    pub configs: Vec<PathBuf>,
    /// The input values of the run.
    pub inputs: &'a Map<String, Json>,
    pub launch: &'a Launch,
    /// A working folder of its own.
    pub folder: PathBuf,
    /// The folder of the results of the run.
    pub output: PathBuf,
    pub files: Option<Arc<dyn Files>>,
    pub cancel: Option<CancellationToken>,
}

type Opener = dyn Fn(&Opening) -> Result<Box<dyn TaskAdapter>, DriverError> + Send + Sync;

/// The adapters that run dialect tasks, by builder name. Tasks of other builders are refused.
#[derive(Clone, Default)]
pub struct Adapters(BTreeMap<String, Arc<Opener>>);

impl Adapters {
    /// Runs the tasks of `builder`, like `snakemake_job`, through adapters that `open` makes,
    /// one for each source with its profiles and configs.
    pub fn register(
        &mut self,
        builder: &str,
        open: impl Fn(&Opening) -> Result<Box<dyn TaskAdapter>, DriverError> + Send + Sync + 'static,
    ) {
        self.0.insert(builder.to_owned(), Arc::new(open));
    }

    pub fn knows(&self, builder: &TaskBuilder) -> bool {
        self.0.contains_key(builder_name(builder))
    }

    pub(crate) fn open(&self, opening: &Opening) -> Result<Box<dyn TaskAdapter>, DriverError> {
        let name = builder_name(opening.builder);
        let open = (self.0.get(name))
            .ok_or_else(|| DriverError::unsupported(format!("uwe runs no {name} tasks")))?;
        open(opening)
    }
}

impl std::fmt::Debug for Adapters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.0.keys()).finish()
    }
}

/// The name of a builder in the format, like `nextflow_process`.
pub fn builder_name(builder: &TaskBuilder) -> &'static str {
    match builder {
        TaskBuilder::NextflowProcess(_) => "nextflow_process",
        TaskBuilder::SnakemakeJob(_) => "snakemake_job",
        TaskBuilder::ArgoTask(_) => "argo_task",
        TaskBuilder::WdlTask(_) => "wdl_task",
    }
}

/// The builder whose adapter plans a region: the job adapter of the planner's source context.
pub(crate) fn planner_builder(planner: &Planner) -> TaskBuilder {
    let Planner::Snakemake(reference) = planner;
    TaskBuilder::SnakemakeJob(wir::RuleTask {
        task: wir::TaskRef {
            source: reference.source.clone(),
            entry: reference.targets.first().cloned().unwrap_or_default(),
            call: String::new(),
            scope: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            profiles: reference.profiles.clone(),
            configs: reference.configs.clone(),
            captures: BTreeMap::new(),
        },
        wildcards: BTreeMap::new(),
        group: Vec::new(),
        targets: reference.targets.clone(),
        config_values: None,
        until: Vec::new(),
        omit_from: Vec::new(),
        forcerun: Vec::new(),
        forceall: false,
        layout: None,
    })
}

/// The frame of a node: its paths and the round numbers of the rounds around it.
pub(crate) fn frame(progress: &Progress, at: usize) -> Frame {
    let node = progress.node(at);
    let mut rounds = Vec::new();
    let mut inner = at;
    while let Some(outer) = progress.node(inner).parent {
        let holder = progress.node(outer);
        let name = (progress.node(inner).path)
            .strip_prefix(&format!("{}/", holder.path))
            .filter(|_| matches!(holder.op.kind, Kind::Map(_) | Kind::Process(_)));
        let numbers = name.into_iter().flat_map(|name| name.split('/').rev());
        rounds.extend(numbers.filter_map(|part| part.parse::<u64>().ok()));
        inner = outer;
    }
    rounds.reverse();
    Frame {
        path: node.path.clone(),
        step: node.step.clone(),
        rounds,
    }
}

/// Which adapter a dialect task runs through: its builder, source, profiles and configs.
pub(crate) type SourceKey = (&'static str, String, Vec<String>, Vec<String>);

/// An adapter that a run opened, under the task ids of the run.
struct Opened {
    adapter: Box<dyn TaskAdapter>,
    /// The run id of each task id of the adapter, and back.
    outer: HashMap<TaskId, TaskId>,
    inner: HashMap<TaskId, TaskId>,
    accepted: HashSet<(TaskId, u32)>,
    /// Deliveries handed to the run and not acknowledged yet.
    delivering: Vec<Delivery>,
    /// Deliveries that its end gave, which the run has not asked for yet.
    held: Vec<Delivery>,
}

/// The adapters a run opened, and the op instances and tasks of each.
#[derive(Default)]
pub(crate) struct Dialects {
    opened: Vec<Opened>,
    keys: HashMap<SourceKey, usize>,
    /// The adapter and the call of each task, by run id.
    owners: HashMap<TaskId, usize>,
    calls_of: HashMap<TaskId, CallId>,
    /// The calls that have not ended, with their adapter.
    calls: BTreeMap<CallId, usize>,
    ended: bool,
}

impl Dialects {
    pub fn find(&self, key: &SourceKey) -> Option<usize> {
        self.keys.get(key).copied()
    }

    /// Adds the adapter of a source context; its index.
    pub fn add(&mut self, key: SourceKey, adapter: Box<dyn TaskAdapter>) -> usize {
        self.opened.push(Opened {
            adapter,
            outer: HashMap::new(),
            inner: HashMap::new(),
            accepted: HashSet::new(),
            delivering: Vec::new(),
            held: Vec::new(),
        });
        self.keys.insert(key, self.opened.len() - 1);
        self.opened.len() - 1
    }

    /// Whether the adapter of a task puts the op path before its name.
    pub fn prefixed(&self, task: TaskId) -> bool {
        let index = self.owners.get(&task);
        index.is_none_or(|index| self.opened[*index].adapter.prefixed())
    }

    pub fn start(
        &mut self,
        index: usize,
        call: &TaskCall,
        next: &mut dyn FnMut() -> TaskId,
    ) -> Result<Changes, DriverError> {
        self.calls.insert(call.call, index);
        let changes = self.opened[index].adapter.start(call);
        if changes.is_err() {
            self.calls.remove(&call.call);
        }
        self.number(index, changes?, next)
    }

    /// Each task by run id, with the node of its call.
    pub fn tasks(&self) -> impl Iterator<Item = (TaskId, usize)> + '_ {
        self.calls_of.iter().map(|(task, call)| (*task, call.0))
    }

    pub fn owns(&self, task: TaskId) -> bool {
        self.owners.contains_key(&task)
    }

    /// Hands an outcome to the adapter of its task; its acceptance is kept before anything
    /// else can fail.
    pub fn complete(
        &mut self,
        mut outcome: Outcome,
        next: &mut dyn FnMut() -> TaskId,
    ) -> Result<Changes, DriverError> {
        let (task, attempt) = (outcome.id, outcome.attempt);
        let Some(&index) = self.owners.get(&task) else {
            return Err(DriverError::new(format!(
                "task {task} is not a dialect task"
            )));
        };
        let opened = &mut self.opened[index];
        outcome.id = opened.inner[&task];
        let changes = opened.adapter.complete(outcome);
        if opened.adapter.take_acceptance(opened.inner[&task], attempt) {
            opened.accepted.insert((task, attempt));
        }
        self.number(index, changes?, next)
    }

    pub fn started(&mut self, task: TaskId, attempt: u32) {
        if let Some(&index) = self.owners.get(&task) {
            let opened = &mut self.opened[index];
            opened.adapter.started(opened.inner[&task], attempt);
        }
    }

    pub fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        let index = self.owners.get(&task).copied();
        index.is_some_and(|index| self.opened[index].accepted.remove(&(task, attempt)))
    }

    pub fn handles_error(&self, task: TaskId, error: &ExecuteError) -> bool {
        self.owners.get(&task).is_some_and(|&index| {
            let opened = &self.opened[index];
            opened.adapter.handles_error(opened.inner[&task], error)
        })
    }

    pub fn wake_at(&self) -> Option<Instant> {
        let times = self
            .opened
            .iter()
            .filter_map(|opened| opened.adapter.wake_at());
        times.min().filter(|_| !self.ended)
    }

    /// Wakes the adapters whose time came.
    pub fn wake(&mut self, next: &mut dyn FnMut() -> TaskId) -> Result<Changes, DriverError> {
        let mut all = Changes::default();
        for index in 0..self.opened.len() {
            let adapter = &mut self.opened[index].adapter;
            if self.ended || adapter.wake_at().is_none_or(|time| time > Instant::now()) {
                continue;
            }
            let changes = adapter.wake()?;
            let changes = self.number(index, changes, next)?;
            all.tasks.extend(changes.tasks);
            all.ended.extend(changes.ended);
            all.ready.extend(changes.ready);
        }
        Ok(all)
    }

    pub fn cancellations(&mut self) -> Vec<TaskId> {
        let mut tasks = Vec::new();
        for opened in &mut self.opened {
            let stopped = opened.adapter.cancellations();
            tasks.extend(
                stopped
                    .iter()
                    .filter_map(|id| opened.outer.get(id).copied()),
            );
        }
        tasks
    }

    pub fn deliveries(&mut self) -> Vec<Delivery> {
        let mut deliveries = Vec::new();
        for opened in &mut self.opened {
            let mut given = std::mem::take(&mut opened.held);
            given.extend(opened.adapter.deliveries());
            opened.delivering.extend(given.iter().cloned());
            deliveries.extend(given);
        }
        deliveries
    }

    /// What the adapters noticed, with the tasks that notes name under their run ids.
    pub fn notes(&mut self) -> Vec<Note> {
        let mut notes = Vec::new();
        for opened in &mut self.opened {
            notes.extend(
                opened
                    .adapter
                    .notes()
                    .into_iter()
                    .filter_map(|note| match note {
                        Note::Mark(task, marker) => {
                            (opened.outer.get(&task)).map(|id| Note::Mark(*id, marker))
                        }
                        other => Some(other),
                    }),
            );
        }
        notes
    }

    pub fn plan(&mut self, index: usize, region: &RegionCall) -> Result<PlanUpdate, DriverError> {
        self.opened[index].adapter.plan(region)
    }

    pub fn replan(
        &mut self,
        index: usize,
        region: CallId,
        ended: (&Identity, Status),
    ) -> Result<Option<PlanUpdate>, DriverError> {
        self.opened[index].adapter.replan(region, ended.0, ended.1)
    }

    /// The deliveries that adapters gave at their end, which the run has not handed out yet.
    pub fn held(&self) -> impl Iterator<Item = &Delivery> + '_ {
        self.opened.iter().flat_map(|opened| &opened.held)
    }

    /// Whether a delivery that an adapter gave waits for its acknowledgement.
    pub fn delivering(&self) -> bool {
        self.opened
            .iter()
            .any(|opened| !opened.delivering.is_empty())
    }

    /// Acknowledges a delivery to the adapter that gave it; `None` when none gave it.
    pub fn delivered(&mut self, delivery: &Delivery) -> Option<Result<(), DriverError>> {
        self.opened.iter_mut().find_map(|opened| {
            let index = opened.delivering.iter().position(|item| item == delivery)?;
            opened.delivering.remove(index);
            Some(opened.adapter.delivered(delivery))
        })
    }

    /// The calls that have not ended, by node.
    pub fn open_calls(&mut self) -> Vec<usize> {
        let calls = std::mem::take(&mut self.calls);
        calls.into_keys().map(|call| call.0).collect()
    }

    /// Ends every adapter, once, with why the run failed; the problems of all, joined.
    pub fn end(&mut self, failure: Option<&str>) -> Result<(), DriverError> {
        if std::mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        let mut problems = Vec::new();
        for opened in &mut self.opened {
            match opened.adapter.end(failure) {
                Ok(given) => opened.held.extend(given),
                Err(error) => problems.push(error.message),
            }
        }
        match problems.is_empty() {
            true => Ok(()),
            false => Err(DriverError::new(problems.join("; "))),
        }
    }

    /// Gives the tasks of an adapter run ids: the same one again for a task it asked for
    /// before, a new one from `next` otherwise.
    fn number(
        &mut self,
        index: usize,
        changes: Changes,
        next: &mut dyn FnMut() -> TaskId,
    ) -> Result<Changes, DriverError> {
        let opened = &mut self.opened[index];
        let mut tasks = Vec::with_capacity(changes.tasks.len());
        for (call, mut task) in changes.tasks {
            let id = *opened.outer.entry(task.id).or_insert_with(|| {
                let id = next();
                opened.inner.insert(id, task.id);
                id
            });
            for after in &mut task.after {
                *after = *opened.outer.get(after).ok_or_else(|| {
                    let message = format!("a dialect task runs after task {after}, unknown here");
                    DriverError::new(message)
                })?;
            }
            task.id = id;
            self.owners.insert(id, index);
            self.calls_of.insert(id, call);
            tasks.push((call, task));
        }
        for (call, _) in &changes.ended {
            self.calls.remove(call);
        }
        Ok(Changes {
            tasks,
            ended: changes.ended,
            ready: changes.ready,
        })
    }
}
