//! Snakemake native blocks, a child driver in a folder of its own with the op inputs as config,
//! and the adapter of `snakemake_job` tasks, which runs compiled jobs through one session.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use serde_json::Value as Json;
use snakemake_runtime::{
    Answer, JobResult, PlannedJob, Prepared, RunSettings, Settings, SnakemakeDriver,
};
use wir::{RuleTask, TaskBuilder};
use workflow_graph::{Delivery, Driver, DriverError, ExecuteError, Launch, Outcome, Task, TaskId};

use crate::adapter::{Adapters, CallId, Changes, Ending, Opening, TaskAdapter, TaskCall};
use crate::child::Block;

/// What the drivers of Snakemake blocks use, as a Snakemake run alone does.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub settings: Settings,
    /// The options of a run; each block starts in a folder of its own instead of `workdir`.
    pub run: RunSettings,
}

/// A Snakemake driver for the launch of one block, which starts in `folder`.
pub(crate) fn block(
    launch: &Launch,
    folder: &Path,
    options: &Options,
) -> Result<Box<dyn Block>, DriverError> {
    let run = RunSettings {
        workdir: Some(folder.to_owned()),
        ..options.run.clone()
    };
    let settings = options.settings.clone();
    Ok(Box::new(snakemake_runtime::spawn_driver(
        launch, settings, run,
    )?))
}

impl Block for SnakemakeDriver {
    /// The files of the targets, which a simulated run names too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(SnakemakeDriver::exported(self))
    }
}

/// Runs `snakemake_job` tasks through one Snakemake session per source context, with `options`.
pub fn register(adapters: &mut Adapters, options: Options) {
    adapters.register("snakemake_job", move |opening| {
        Ok(Box::new(Jobs::open(opening, &options)?) as Box<dyn TaskAdapter>)
    });
}

/// A call of the job adapter, which commands of workflow code may hold up.
enum Step {
    Prepare(CallId, Box<RuleTask>, Vec<(String, Json)>),
    Take(Box<Outcome>),
}

/// The jobs of a compiled plan, each prepared and taken through the job adapter of one
/// session, which plans as the run that the document was compiled for.
struct Jobs {
    driver: SnakemakeDriver,
    /// The planned jobs, once the session started them.
    planned: Option<Vec<PlannedJob>>,
    /// The call whose commands run, the steps after it, and the job tasks that start once no
    /// command waits, as a native run hands them out.
    held: Option<CallId>,
    commands: HashSet<TaskId>,
    steps: VecDeque<Step>,
    ready: Vec<(CallId, Task)>,
    /// The call of each job task, and the output port of each output selector of a call.
    owners: HashMap<TaskId, CallId>,
    ports: HashMap<CallId, HashMap<String, String>>,
}

impl Jobs {
    fn open(opening: &Opening, options: &Options) -> Result<Jobs, DriverError> {
        let TaskBuilder::SnakemakeJob(rule) = opening.builder else {
            return Err(DriverError::new("not a Snakemake job"));
        };
        // Config values live in a source of the document, which the run names.
        let values = match &rule.config_values {
            Some(_) => {
                let text = std::fs::read_to_string(&opening.launch.target)
                    .map_err(|error| DriverError::new(error.to_string()))?;
                let document =
                    wir::parse(&text).map_err(|error| DriverError::new(error.to_string()))?;
                let base = opening.launch.target.parent().unwrap_or(Path::new("."));
                snakemake_compiler::rule_values(&document, rule, base).map_err(DriverError::new)?
            }
            None => Vec::new(),
        };
        let run = Launch {
            target: opening.source.clone(),
            configs: opening.configs.clone(),
            inputs: None,
            ..opening.launch.clone()
        };
        let launch = snakemake_compiler::plan_launch(rule, values, run);
        let mut driver = snakemake_runtime::spawn_driver(
            &launch,
            options.settings.clone(),
            options.run.clone(),
        )?;
        if let Some(files) = &opening.files {
            driver.use_files(files.clone());
        }
        Ok(Jobs {
            driver,
            planned: None,
            held: None,
            commands: HashSet::new(),
            steps: VecDeque::new(),
            ready: Vec::new(),
            owners: HashMap::new(),
            ports: HashMap::new(),
        })
    }

    /// Runs the steps that wait while no command holds them up.
    fn drive(&mut self, mut changes: Changes) -> Result<Changes, DriverError> {
        while self.held.is_none() {
            let Some(step) = self.steps.pop_front() else {
                changes.tasks.append(&mut self.ready);
                break;
            };
            let (call, answer) = match step {
                Step::Prepare(call, rule, staged) => {
                    let planned = self.planned.as_deref().unwrap_or_default();
                    if !snakemake_compiler::plans_job(planned, &rule) {
                        let why = format!(
                            "the run plans no job of rule {} as compiled",
                            rule.task.call
                        );
                        changes.ended.push((call, Ending::Errored(why)));
                        continue;
                    }
                    let key = snakemake_compiler::job_key(&rule);
                    let group = snakemake_compiler::group_keys(&rule);
                    (call, self.driver.prepare_job(&key, &group, staged)?)
                }
                Step::Take(outcome) => (self.owners[&outcome.id], self.driver.take_job(*outcome)?),
            };
            self.answered(call, answer, &mut changes);
        }
        Ok(changes)
    }

    /// Takes what a call of `call` gave into `changes`.
    fn answered(&mut self, call: CallId, answer: Answer, changes: &mut Changes) {
        match answer {
            Answer::Commands(commands) => {
                self.held = Some(call);
                self.commands.extend(commands.iter().map(|task| task.id));
                changes
                    .tasks
                    .extend(commands.into_iter().map(|task| (call, task)));
            }
            Answer::Jobs(jobs) => self.planned = Some(jobs),
            Answer::Targets(_) => {}
            Answer::Prepared(Prepared::Run(task)) | Answer::Taken(JobResult::Retry(task)) => {
                self.owners.insert(task.id, call);
                self.ready.push((call, *task));
            }
            Answer::Prepared(Prepared::Tasks(tasks)) | Answer::Taken(JobResult::Tasks(tasks)) => {
                self.owners.extend(tasks.iter().map(|task| (task.id, call)));
                self.ready
                    .extend(tasks.into_iter().map(|task| (call, task)));
            }
            Answer::Prepared(Prepared::Skipped(made))
            | Answer::Taken(JobResult::Accepted(made)) => {
                let ports = self.ports.remove(&call).unwrap_or_default();
                let values = (made.into_iter().enumerate()).filter_map(|(index, file)| {
                    Some((ports.get(&format!("output/{index}"))?.clone(), file))
                });
                let ending = Ending::Succeeded {
                    values: values.collect(),
                    empty: Vec::new(),
                };
                changes.ended.push((call, ending));
            }
            Answer::Prepared(Prepared::Failed(why)) | Answer::Taken(JobResult::Failed(why)) => {
                changes.ended.push((call, Ending::Failed(why)));
            }
        }
    }
}

impl TaskAdapter for Jobs {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let TaskBuilder::SnakemakeJob(rule) = call.builder else {
            return Err(DriverError::new("not a Snakemake job"));
        };
        let mut changes = Changes::default();
        if self.planned.is_none() && self.held.is_none() {
            let answer = self.driver.start_jobs()?;
            self.answered(call.call, answer, &mut changes);
        }
        let reference = &rule.task;
        let staged = (reference.inputs.iter())
            .filter_map(|map| Some((map.native.clone(), call.inputs.get(&map.port)?.clone())))
            .collect();
        let ports = (reference.outputs.iter()).map(|map| (map.native.clone(), map.port.clone()));
        self.ports.insert(call.call, ports.collect());
        let rule = Box::new(rule.clone());
        self.steps.push_back(Step::Prepare(call.call, rule, staged));
        self.drive(changes)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let mut changes = Changes::default();
        if self.commands.remove(&outcome.id) {
            let call = self
                .held
                .take()
                .ok_or_else(|| DriverError::new("no call waits"))?;
            let answer = self.driver.take_job(outcome)?;
            self.answered(call, answer, &mut changes);
        } else {
            self.steps.push_back(Step::Take(Box::new(outcome)));
        }
        self.drive(changes)
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.driver.take_acceptance(task, attempt)
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        self.driver.deliveries()
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.driver.delivered(delivery)
    }

    fn end(&mut self, failure: Option<&str>) -> Result<Vec<Delivery>, DriverError> {
        let ended = match failure {
            None => self.driver.finish(),
            Some(why) => self
                .driver
                .failed(&ExecuteError::Driver(DriverError::new(why))),
        };
        // The deliveries of its end come through `deliveries`, as those of its tasks do.
        ended.map(|()| Vec::new())
    }

    /// Job tasks keep the names of the native run, which name the rule and wildcards.
    fn prefixed(&self) -> bool {
        false
    }
}
