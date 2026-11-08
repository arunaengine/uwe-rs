//! Runs single planned jobs for dialect tasks of wir, through the job preparation and the
//! accepted completion of the native driver, with each job named by its rule and wildcards.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use python_evaluator::Value;
use serde_json::{Value as Json, json};
use workflow_graph::{DriverError, Outcome, Task};

use super::{Session, SnakemakeDriver, Taken};
use crate::commands::{Reply, Request};
use crate::files::Produced;
use crate::rule::Action;

/// The identity of a planned job: its rule, its wildcard values and the files it makes.
/// Positions in the plan are no identity, since a new plan may order jobs differently.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobKey {
    pub rule: String,
    pub wildcards: BTreeMap<String, String>,
    pub outputs: Vec<String>,
}

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
    /// Jobs that end before it besides its producers: checkpoints its input functions read.
    pub after: Vec<usize>,
    pub checkpoint: bool,
    /// Whether a target of the run names the job, like the first rule without targets.
    pub target: bool,
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
    Skipped(Vec<Json>),
    Failed(String),
}

/// How the outcome of the task of a job was taken: the files it made by output index, the
/// task of its next attempt, or why it failed.
#[derive(Clone, Debug, PartialEq)]
pub enum JobResult {
    Accepted(Vec<Json>),
    Retry(Box<Task>),
    Failed(String),
}

/// What the session answers to a request of a job, beside its reply.
pub(crate) enum Answer {
    Jobs(Vec<PlannedJob>),
    Prepared(Prepared),
    Taken(JobResult),
}

pub(crate) type Answers = Arc<Mutex<Option<Answer>>>;

impl SnakemakeDriver {
    /// The planned jobs of the workflow, as a run would plan them, without starting the run.
    pub fn plan_jobs(&mut self) -> Result<Vec<PlannedJob>, DriverError> {
        match self.ask_job(Request::Jobs(false))? {
            Answer::Jobs(jobs) => Ok(jobs),
            _ => Err(DriverError::new("the Snakemake session gave no jobs")),
        }
    }

    /// Starts a run whose jobs run one by one through [`SnakemakeDriver::prepare_job`]: plans
    /// the jobs, takes the lock and runs `onstart`, as a native run does before its tasks.
    pub fn start_jobs(&mut self) -> Result<Vec<PlannedJob>, DriverError> {
        match self.ask_job(Request::Jobs(true))? {
            Answer::Jobs(jobs) => Ok(jobs),
            _ => Err(DriverError::new("the Snakemake session gave no jobs")),
        }
    }

    /// The first task of the job of `key`, with input files given as wir values by their
    /// selectors, like `input/0`; the task the native run builds for that job.
    pub fn prepare_job(
        &mut self,
        key: &JobKey,
        staged: Vec<(String, Json)>,
    ) -> Result<Prepared, DriverError> {
        match self.ask_job(Request::Prepare(Box::new((key.clone(), staged))))? {
            Answer::Prepared(prepared) => Ok(prepared),
            _ => Err(DriverError::new("the Snakemake session prepared no job")),
        }
    }

    /// Takes the outcome of a task that [`SnakemakeDriver::prepare_job`] gave, or a retry.
    pub fn take_job(&mut self, outcome: Outcome) -> Result<JobResult, DriverError> {
        match self.ask_job(Request::Take(Box::new(outcome)))? {
            Answer::Taken(result) => Ok(result),
            _ => Err(DriverError::new("the Snakemake session took no outcome")),
        }
    }

    fn ask_job(&mut self, request: Request) -> Result<Answer, DriverError> {
        let tasks = self.ask(request)?;
        if !tasks.is_empty() {
            return Err(DriverError::new(
                "workflow code ran a command while a job was planned or prepared",
            ));
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
                let (key, staged) = *request;
                Answer::Prepared(self.prepare_key(&key, &staged)?)
            }
            Request::Take(outcome) => Answer::Taken(self.take_key(*outcome)?),
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
        let position: HashMap<usize, usize> = (self.order.iter().enumerate())
            .map(|(position, job)| (*job, position))
            .collect();
        self.keys.clear();
        let mut jobs = Vec::new();
        for job in &self.order {
            let entry = &planner.jobs[*job];
            let rule = &planner.rules[entry.rule];
            let key = job_key(planner, *job);
            let named = JobKey {
                outputs: Vec::new(),
                ..key.clone()
            };
            self.keys.insert(named, *job);
            let inputs = entry.input_files();
            let mut producers = Vec::new();
            for (file, producer) in &entry.dependencies {
                let made = planner.jobs[*producer].output_files();
                let (Some(input), Some(output), Some(job)) = (
                    inputs.iter().position(|input| input == file),
                    made.iter().position(|output| output == file),
                    position.get(producer),
                ) else {
                    continue;
                };
                let producer = Producer {
                    input,
                    job: *job,
                    output,
                };
                if !producers.contains(&producer) {
                    producers.push(producer);
                }
            }
            let after = (entry.checkpoints.iter())
                .filter_map(|file| {
                    let maker = self
                        .order
                        .iter()
                        .find(|other| planner.jobs[**other].output_files().contains(file))?;
                    position.get(maker).copied()
                })
                .collect();
            let grouped = self.components.get(job).is_some_and(|jobs| jobs.len() > 1);
            let native = if entry.incomplete {
                Some("it waits for a checkpoint".to_owned())
            } else if grouped {
                Some("it runs in one task with other jobs, as a group, pipe or service".into())
            } else if !crate::services::service_files(entry).is_empty() {
                Some("it has service outputs".to_owned())
            } else if matches!(rule.action, Action::Cwl(_)) {
                Some("it runs a CWL document".to_owned())
            } else {
                None
            };
            jobs.push(PlannedJob {
                key,
                needed: self.needrun.get(job).copied().unwrap_or(false),
                inputs,
                outputs: job_files(entry),
                producers,
                after,
                checkpoint: rule.checkpoint,
                target: self.roots.contains(job),
                native,
            });
        }
        Ok(jobs)
    }

    fn prepare_key(
        &mut self,
        key: &JobKey,
        staged: &[(String, Json)],
    ) -> Result<Prepared, DriverError> {
        let job = self.job_of(key)?;
        if !self.needrun.get(&job).copied().unwrap_or(false) {
            return Ok(Prepared::Skipped(self.made_files(job)));
        }
        if self.tasks.contains_key(&job) {
            return Err(DriverError::new(format!(
                "the job of rule {} has started already",
                key.rule
            )));
        }
        let planner = (self.planner.as_ref())
            .ok_or_else(|| DriverError::new("the Snakemake session has not planned its jobs"))?;
        let inputs = planner.jobs[job].input_files();
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
        let task = self.prepare_job(job)?;
        if self.cwl_jobs.remove(&job).is_some() {
            let message = format!("the job of rule {} runs a CWL document", key.rule);
            return Err(DriverError::new(message));
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
        self.unplace(&mut outcome)?;
        let identity = (outcome.id, outcome.attempt);
        let Some(job) = self.jobs_of_tasks.get(&outcome.id).copied() else {
            return Err(DriverError::new(format!("unknown task {}", outcome.id)));
        };
        Ok(match self.take_job(job, outcome)? {
            Taken::Retry(task) => JobResult::Retry(task),
            Taken::Failed(message) => {
                self.failures.push(message.clone());
                JobResult::Failed(message)
            }
            Taken::Accepted => {
                self.accept(identity);
                JobResult::Accepted(self.made_files(job))
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
