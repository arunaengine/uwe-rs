//! Runs single planned jobs for dialect tasks of wir, through the job preparation and the
//! accepted completion of the native driver, with each job named by its rule and wildcards.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use python_evaluator::Value;
use serde_json::{Value as Json, json};
use workflow_graph::{DriverError, Outcome, Task, TaskId};

use super::{Session, SnakemakeDriver, Taken};
use crate::commands::{Reply, Request};
use crate::files::Produced;

/// The identity of a planned job: its rule, its wildcard values and the files it makes.
/// Positions in the plan are no identity, since a new plan may order jobs differently.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobKey {
    pub rule: String,
    pub wildcards: BTreeMap<String, String>,
    pub outputs: Vec<String>,
}

/// A call to prepare a job: its identity, the other jobs of its group as compiled, and its
/// input files as wir values by their selectors.
pub(crate) type JobCall = (JobKey, Vec<JobKey>, Vec<(String, Json)>);

/// A planned job, in the order a native run asks for the tasks of jobs.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedJob {
    pub key: JobKey,
    /// Whether the job runs, as Snakemake decides from the files that are there.
    pub needed: bool,
    /// The input files, each once, which the selector `input/<index>` names.
    pub inputs: Vec<String>,
    /// The files the job makes, which the selector `output/<index>` names: its outputs, then
    /// its logs, then its benchmark.
    pub outputs: Vec<JobFile>,
    /// The inputs that other jobs make, by input index, with that job and its output index.
    pub producers: Vec<Producer>,
    /// Jobs that end before it besides its producers: checkpoints its input functions read,
    /// and the jobs that read a file of its `update()` outputs as it was before.
    pub after: Vec<usize>,
    pub checkpoint: bool,
    /// The rule of its job or group job that a target of the run names, like the first rule
    /// without targets.
    pub target: Option<String>,
    /// The other jobs that run in its task, as a group job or a pipe; the inputs are then the
    /// files that no job of the group makes, and the outputs those of each job in order.
    pub group: Vec<JobKey>,
    /// Why one task of its own cannot run the job, like a group job; `None` when one can.
    pub native: Option<String>,
}

/// A file that a job makes, with the flags of the Snakefile, like `temp` or `protected`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobFile {
    pub path: String,
    pub kind: FileKind,
    pub flags: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Output,
    Log,
    Benchmark,
}

/// An input of a job that another job of the plan makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Producer {
    pub input: usize,
    /// The position of the producing job in the plan.
    pub job: usize,
    pub output: usize,
}

/// The first task of a job, or the files of a job that does not run, by output index.
#[derive(Clone, Debug, PartialEq)]
pub enum Prepared {
    Run(Box<Task>),
    /// The tasks of the CWL run of a `cwl:` job, whose outcomes end the job.
    Tasks(Vec<Task>),
    Skipped(Vec<Json>),
    Failed(String),
}

/// How the outcome of the task of a job was taken: the files it made by output index, the
/// task of its next attempt, or why it failed.
#[derive(Clone, Debug, PartialEq)]
pub enum JobResult {
    Accepted(Vec<Json>),
    Retry(Box<Task>),
    /// More tasks of the CWL run of a `cwl:` job, maybe none while others run.
    Tasks(Vec<Task>),
    Failed(String),
}

/// What a call of the job adapter gave: its answer, or commands of workflow code that run
/// first, whose outcomes go to [`SnakemakeDriver::take_job`] before any other call.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Jobs(Vec<PlannedJob>),
    Prepared(Prepared),
    Taken(JobResult),
    Commands(Vec<Task>),
    /// The files of the targets by output name, as a finished native run names them, in
    /// the working folder.
    Targets(Json),
}

pub(crate) type Answers = Arc<Mutex<Option<Answer>>>;

impl SnakemakeDriver {
    /// The planned jobs of the workflow, as a run would plan them, without starting the run;
    /// commands of workflow code run at once, as hooks do.
    pub fn plan_jobs(&mut self) -> Result<Vec<PlannedJob>, DriverError> {
        match self.ask_job(Request::Jobs(false))? {
            Answer::Jobs(jobs) => Ok(jobs),
            _ => Err(DriverError::new("the Snakemake session gave no jobs")),
        }
    }

    /// The files of the targets of the jobs that [`SnakemakeDriver::plan_jobs`] planned, by
    /// the names of the outputs of a run.
    pub fn plan_targets(&mut self) -> Result<Json, DriverError> {
        match self.ask_job(Request::Targets)? {
            Answer::Targets(targets) => Ok(targets),
            _ => Err(DriverError::new("the Snakemake session gave no targets")),
        }
    }

    /// Starts a run whose jobs run one by one through [`SnakemakeDriver::prepare_job`]: plans
    /// the jobs, takes the lock and runs `onstart`, as a native run does before its tasks.
    /// It gives the jobs, or commands first.
    pub fn start_jobs(&mut self) -> Result<Answer, DriverError> {
        self.ask_job(Request::Jobs(true))
    }

    /// The first task of the job of `key`, with the other jobs of its group as compiled and
    /// input files given as wir values by their selectors, like `input/0`; the task the native
    /// run builds for that job, or commands.
    pub fn prepare_job(
        &mut self,
        key: &JobKey,
        group: &[JobKey],
        staged: Vec<(String, Json)>,
    ) -> Result<Answer, DriverError> {
        let call = (key.clone(), group.to_vec(), staged);
        self.ask_job(Request::Prepare(Box::new(call)))
    }

    /// Takes the outcome of a task that a call gave: of a job, which gives the result or a
    /// retry, or of a command, which gives more commands or the answer of that call.
    pub fn take_job(&mut self, outcome: Outcome) -> Result<Answer, DriverError> {
        let request = match self.commands.remove(&outcome.id) {
            true => Request::Complete(Box::new(outcome)),
            false => Request::Take(Box::new(outcome)),
        };
        self.ask_job(request)
    }

    fn ask_job(&mut self, request: Request) -> Result<Answer, DriverError> {
        let tasks = self.ask(request)?;
        if !tasks.is_empty() {
            self.commands.extend(tasks.iter().map(|task| task.id));
            return Ok(Answer::Commands(tasks));
        }
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers
            .take()
            .ok_or_else(|| DriverError::new("the Snakemake session did not answer"))
    }
}

impl Session {
    /// Answers a request of a job in the slot of answers.
    pub(crate) fn answer(&mut self, request: Request, answers: &Answers) -> Reply {
        let answer = match request {
            Request::Jobs(start) => Answer::Jobs(self.planned_jobs(start)?),
            Request::Prepare(request) => {
                let (key, group, staged) = *request;
                Answer::Prepared(self.prepare_key(&key, &group, &staged)?)
            }
            Request::Take(outcome) => Answer::Taken(self.take_key(*outcome)?),
            Request::Targets => Answer::Targets(self.target_files()),
            _ => return Err(DriverError::new("not a request of a job")),
        };
        *answers.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
        Ok(Vec::new())
    }

    fn planned_jobs(&mut self, start: bool) -> Result<Vec<PlannedJob>, DriverError> {
        self.evaluate_once()?;
        self.sources = std::rc::Rc::new(crate::sources::Sources::find(&self.state.borrow()));
        self.plan()?;
        if start {
            self.begin_run()?;
        }
        let planner = (self.planner.as_ref())
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        // Each planned job with the jobs of its task, and where each job and its files are.
        let mut planned: Vec<Vec<usize>> = Vec::new();
        let mut position: HashMap<usize, (usize, usize)> = HashMap::new();
        for job in &self.order {
            if position.contains_key(job) {
                continue;
            }
            let members = self.members(*job);
            let mut offset = 0;
            for member in &members {
                position.insert(*member, (planned.len(), offset));
                offset += planner.jobs[*member].output_files().len();
            }
            planned.push(members);
        }
        // A compiler learns which groups a run splits since their jobs run in several images.
        let split: Vec<bool> = match start {
            true => vec![false; planned.len()],
            false => (planned.iter())
                .map(|members| members.len() > 1 && self.split_images(members))
                .collect(),
        };
        let planner = (self.planner.as_ref())
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        self.keys.clear();
        let mut jobs = Vec::new();
        for (members, split) in planned.iter().zip(split) {
            let job = members[0];
            let key = job_key(planner, job);
            let named = JobKey {
                outputs: Vec::new(),
                ..key.clone()
            };
            self.keys.insert(named, job);
            let inputs = group_inputs(planner, members);
            let mut producers = Vec::new();
            let mut after = Vec::new();
            for entry in members.iter().map(|member| &planner.jobs[*member]) {
                // Jobs of the group make files for each other, not for the group.
                let outside = (entry.dependencies.iter()).filter(|(_, job)| !members.contains(job));
                for (file, producer) in outside {
                    let made = planner.jobs[*producer].output_files();
                    let (Some(input), Some(output), Some((job, offset))) = (
                        inputs.iter().position(|input| input == file),
                        made.iter().position(|output| output == file),
                        position.get(producer),
                    ) else {
                        continue;
                    };
                    let producer = Producer {
                        input,
                        job: *job,
                        output: offset + output,
                    };
                    if !producers.contains(&producer) {
                        producers.push(producer);
                    }
                }
                after.extend((entry.checkpoints.iter()).filter_map(|file| {
                    let maker = self
                        .order
                        .iter()
                        .find(|other| planner.jobs[**other].output_files().contains(file))?;
                    position.get(maker).map(|(job, _)| *job)
                }));
                // An `update()` waits for the jobs that read the file as it was before.
                let updated: Vec<&str> = (entry.output.values.iter())
                    .filter(|value| value.tag("update").is_some())
                    .filter_map(Value::as_str)
                    .collect();
                let readers = (self.order.iter()).filter(|reader| {
                    !members.contains(reader)
                        && (planner.jobs[**reader].input.values.iter()).any(|value| {
                            value.tag("before_update").is_some()
                                && value.as_str().is_some_and(|file| updated.contains(&file))
                        })
                });
                after
                    .extend(readers.filter_map(|reader| position.get(reader).map(|(job, _)| *job)));
            }
            after.sort_unstable();
            after.dedup();
            let split =
                split.then_some("its group runs jobs of several images, which a run splits");
            let native = split.map(str::to_owned).or_else(|| {
                members.iter().find_map(|member| {
                    let entry = &planner.jobs[*member];
                    if entry.incomplete {
                        Some("it waits for a checkpoint".to_owned())
                    } else if members.len() == 1
                        && !crate::services::service_files(entry).is_empty()
                    {
                        Some("it has service outputs".to_owned())
                    } else {
                        None
                    }
                })
            });
            let needed = |member: &usize| self.needrun.get(member).copied().unwrap_or(false);
            jobs.push(PlannedJob {
                key,
                needed: members.iter().any(needed),
                inputs,
                outputs: (members.iter())
                    .flat_map(|member| job_files(&planner.jobs[*member]))
                    .collect(),
                producers,
                after,
                checkpoint: (members.iter())
                    .any(|member| planner.rules[planner.jobs[*member].rule].checkpoint),
                target: (members.iter())
                    .find(|member| self.roots.contains(member))
                    .map(|member| planner.rules[planner.jobs[*member].rule].name.clone()),
                group: (members[1..].iter())
                    .map(|member| JobKey {
                        outputs: Vec::new(),
                        ..job_key(planner, *member)
                    })
                    .collect(),
                native,
            });
        }
        Ok(jobs)
    }

    /// Whether the jobs of a group build tasks of several images, which a run splits into
    /// separate tasks; also when a task cannot be built while the jobs are planned.
    fn split_images(&mut self, members: &[usize]) -> bool {
        let mut images = HashSet::new();
        for member in members {
            let Some(planner) = self.planner.as_ref() else {
                return true;
            };
            let entry = &planner.jobs[*member];
            let state = self.state.borrow();
            let setting = crate::task::TaskSetting {
                state: &state,
                files: &self.files,
                workdir: &self.workdir,
                embedded: &self.embedded,
                sources: &self.sources,
            };
            let rule = &planner.rules[entry.rule];
            match crate::task::build(&mut self.interpreter, &setting, rule, entry, TaskId(0), 1) {
                Ok(built) => images.insert(built.task.image),
                Err(_) => return true,
            };
        }
        images.len() > 1
    }

    /// The jobs that run in one task with `job`, it first: a service job with the jobs that
    /// use it, a group job or a pipe of the run, else `job` alone.
    fn members(&self, job: usize) -> Vec<usize> {
        if let Some(services) = self.served(job)
            && let Ok((users, _)) = self.service_users(job, &services)
            && !users.is_empty()
        {
            return std::iter::once(job).chain(users).collect();
        }
        match self.components.get(&job) {
            Some(members) if members.len() > 1 => members.to_vec(),
            _ => vec![job],
        }
    }

    /// The service outputs of a job that runs with its users, as a run starts it, outside a
    /// pipe.
    fn served(&self, job: usize) -> Option<Vec<String>> {
        let planner = self.planner.as_ref()?;
        let services = crate::services::service_files(&planner.jobs[job]);
        let piped = (self.components.get(&job))
            .is_some_and(|jobs| !crate::pipes::files(planner, jobs).is_empty());
        (!services.is_empty() && !piped).then_some(services)
    }

    fn prepare_key(
        &mut self,
        key: &JobKey,
        group: &[JobKey],
        staged: &[(String, Json)],
    ) -> Result<Prepared, DriverError> {
        let job = self.job_of(key)?;
        let members = self.members(job);
        let needed = |member: &usize| self.needrun.get(member).copied().unwrap_or(false);
        // A run groups only jobs that need work; a compiled group of current jobs is skipped.
        if members.len() < group.len() + 1 {
            let recorded = std::iter::once(Ok(job)).chain(group.iter().map(|key| self.job_of(key)));
            let recorded: Vec<usize> = recorded.collect::<Result<_, _>>()?;
            if recorded.iter().any(needed) {
                let message = format!("the run groups the job of rule {} otherwise", key.rule);
                return Err(DriverError::new(message));
            }
            return Ok(Prepared::Skipped(self.group_files(&recorded)));
        }
        if !members.iter().any(needed) {
            return Ok(Prepared::Skipped(self.group_files(&members)));
        }
        if members.iter().any(|member| self.tasks.contains_key(member)) {
            return Err(DriverError::new(format!(
                "the job of rule {} has started already",
                key.rule
            )));
        }
        let planner = (self.planner.as_ref())
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let inputs = group_inputs(planner, &members);
        for (selector, value) in staged {
            let file = (selector.strip_prefix("input/"))
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| inputs.get(index));
            let (Some(file), Some(location)) = (file, value["location"].as_str()) else {
                return Err(DriverError::new(format!(
                    "the job of rule {} has no input {selector} with a location",
                    key.rule
                )));
            };
            let path = self.interpreter.host.resolve(file.trim_end_matches('/'));
            let produced = Produced {
                location: location.into(),
                directory: value["class"] == "Directory",
            };
            self.files.add_output(&path, produced);
        }
        if members.len() > 1 {
            let task = match self.served(job) {
                Some(services) => self.service_group(job, &services, None)?,
                None => self.rule_group(&members, None)?,
            };
            return Ok(match task {
                Some(task) => Prepared::Run(Box::new(task)),
                None => Prepared::Failed(self.failures.last().cloned().unwrap_or_else(|| {
                    format!("the group of rule {} did not run in one task", key.rule)
                })),
            });
        }
        let task = self.prepare_job(job)?;
        if let Some(cwl) = self.cwl_jobs.remove(&job) {
            let Some(task) = self.start_job(job, task)? else {
                let failure = self.failures.last().cloned().unwrap_or_default();
                return Ok(Prepared::Failed(failure));
            };
            let tasks = self.start_cwl(job, task.id, cwl)?;
            return Ok(match self.cwl_ended.take() {
                None => Prepared::Tasks(tasks),
                Some(ended) => match self.take_key(ended)? {
                    JobResult::Accepted(made) => Prepared::Skipped(made),
                    JobResult::Failed(message) => Prepared::Failed(message),
                    JobResult::Retry(task) => Prepared::Run(task),
                    JobResult::Tasks(tasks) => Prepared::Tasks(tasks),
                },
            });
        }
        match self.start_job(job, task)? {
            Some(task) => Ok(Prepared::Run(Box::new(task))),
            None => {
                let failure = self.failures.last().cloned().unwrap_or_default();
                Ok(Prepared::Failed(failure))
            }
        }
    }

    fn take_key(&mut self, mut outcome: Outcome) -> Result<JobResult, DriverError> {
        if let Some(job) = self.cwl_tasks.remove(&outcome.id) {
            let more = self.complete_cwl(job, outcome)?;
            return match self.cwl_ended.take() {
                Some(ended) => self.take_key(ended),
                None => Ok(JobResult::Tasks(more)),
            };
        }
        self.unplace(&mut outcome)?;
        let identity = (outcome.id, outcome.attempt);
        let Some(job) = self.jobs_of_tasks.get(&outcome.id).copied() else {
            return Err(DriverError::new(format!("unknown task {}", outcome.id)));
        };
        let (members, taken) = match self.groups.remove(&outcome.id) {
            Some(members) => {
                let jobs = members.iter().map(|(member, _)| *member).collect();
                (jobs, self.take_group(members, outcome)?)
            }
            None => (vec![job], self.take_job(job, outcome)?),
        };
        Ok(match taken {
            Taken::Retry(task) => JobResult::Retry(task),
            Taken::Failed(message) => {
                self.failures.push(message.clone());
                JobResult::Failed(message)
            }
            Taken::Accepted => {
                self.accept(identity);
                JobResult::Accepted(self.group_files(&members))
            }
        })
    }

    /// The job of a key, by its rule and wildcards; the outputs of a key must match when given.
    fn job_of(&self, key: &JobKey) -> Result<usize, DriverError> {
        let named = JobKey {
            outputs: Vec::new(),
            ..key.clone()
        };
        let job = self.keys.get(&named).copied().filter(|job| {
            let planner = self.planner.as_ref();
            key.outputs.is_empty() || planner.is_some_and(|planner| job_key(planner, *job) == *key)
        });
        job.ok_or_else(|| {
            DriverError::new(format!(
                "the plan has no job of rule {} with these wildcards and outputs",
                key.rule
            ))
        })
    }

    /// The files of the jobs of a task, each job's files in turn.
    fn group_files(&self, members: &[usize]) -> Vec<Json> {
        (members.iter())
            .flat_map(|member| self.made_files(*member))
            .collect()
    }

    /// The files of a job as wir values, by output index: where its task left them, else
    /// where they are in the working directory.
    fn made_files(&self, job: usize) -> Vec<Json> {
        let Some(planner) = &self.planner else {
            return Vec::new();
        };
        (planner.jobs[job].output_files().iter())
            .map(|file| {
                let path = self.interpreter.host.resolve(file.trim_end_matches('/'));
                let (location, directory) = self.files.location(&path).unwrap_or_else(|| {
                    let directory = path.is_dir() || file.ends_with('/');
                    (path.clone(), directory)
                });
                let basename = Path::new(file.trim_end_matches('/')).file_name();
                json!({
                    "class": if directory { "Directory" } else { "File" },
                    "location": location.to_string_lossy(),
                    "basename": basename.map(|name| name.to_string_lossy()),
                })
            })
            .collect()
    }
}

/// The input files of the jobs of a task that none of them makes, each once.
fn group_inputs(planner: &crate::plan::Planner, members: &[usize]) -> Vec<String> {
    let made: Vec<String> = (members.iter())
        .flat_map(|member| planner.jobs[*member].output_files())
        .collect();
    let mut inputs: Vec<String> = Vec::new();
    for file in members
        .iter()
        .flat_map(|member| planner.jobs[*member].input_files())
    {
        if !made.contains(&file) && !inputs.contains(&file) {
            inputs.push(file);
        }
    }
    inputs
}

/// The identity of a planned job.
fn job_key(planner: &crate::plan::Planner, job: usize) -> JobKey {
    let entry = &planner.jobs[job];
    JobKey {
        rule: planner.rules[entry.rule].name.clone(),
        wildcards: (entry.wildcards.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        outputs: entry.output_files(),
    }
}

/// The files a job makes, in the order of `Job::output_files`, with their flags.
fn job_files(entry: &crate::plan::Job) -> Vec<JobFile> {
    let flags = |value: &Value| -> Vec<String> {
        (value.tags().into_iter().flat_map(|tags| tags.keys()))
            .map(ToString::to_string)
            .collect()
    };
    let kinds = (entry
        .output
        .values
        .iter()
        .map(|value| (value, FileKind::Output)))
    .chain(entry.log.values.iter().map(|value| (value, FileKind::Log)))
    .chain(
        entry
            .benchmark
            .iter()
            .map(|value| (value, FileKind::Benchmark)),
    );
    kinds
        .filter_map(|(value, kind)| {
            Some(JobFile {
                path: value.as_str()?.to_owned(),
                kind,
                flags: flags(value),
            })
        })
        .collect()
}
