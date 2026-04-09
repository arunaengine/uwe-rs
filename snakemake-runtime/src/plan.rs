//! Plans the jobs of a workflow backwards from its targets, as Snakemake's DAG does: each
//! input file is made by the job of a rule whose output pattern matches it, or is a file that
//! exists. A mock run may assume that input files that no rule can make exist, since raw data
//! is not part of a workflow.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use python_evaluator::{Error, ErrorKind, Interpreter, Value};

use crate::functions::{call_input_function, is_callable};
use crate::objects::NamedList;
use crate::pattern::{self, Matcher, Wildcards};
use crate::rule::{Items, Rule, retag};

/// A job: a rule with wildcard values, and its concrete files.
#[derive(Clone)]
pub struct Job {
    pub rule: usize,
    pub wildcards: Wildcards,
    pub input: Items,
    pub output: Items,
    pub log: Items,
    pub benchmark: Option<Value>,
    /// The job that makes each input file that a job makes.
    pub dependencies: Vec<(String, usize)>,
    /// Whether an input function waits for a checkpoint that has not run yet.
    pub incomplete: bool,
    /// Whether some input file below this job was assumed to exist.
    pub assumed: bool,
    /// The input files of this job that were assumed to exist.
    pub missing: Vec<String>,
    /// The first outputs of the done checkpoints that the input functions asked for.
    pub checkpoints: Vec<String>,
}

impl Job {
    pub fn output_files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for value in self.output.values.iter().chain(self.log.values.iter()) {
            if let Some(text) = value.as_str() {
                files.push(text.to_owned());
            }
        }
        if let Some(benchmark) = self.benchmark.as_ref().and_then(Value::as_str) {
            files.push(benchmark.to_owned());
        }
        files
    }

    pub fn input_files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for value in &self.input.values {
            if let Some(text) = value.as_str()
                && !files.iter().any(|file| file == text)
            {
                files.push(text.to_owned());
            }
        }
        files
    }
}

/// Why planning a file or a job failed.
#[derive(Clone)]
pub enum Failure {
    /// A rule would consume its own output, or its wildcards grow without end.
    Cycle(String),
    /// An error of the workflow, like an input function that raised.
    Error(Error),
}

impl Failure {
    pub fn into_error(self) -> Error {
        match self {
            Failure::Cycle(message) => crate::workflow::workflow_error(message),
            Failure::Error(error) => error,
        }
    }
}

type JobKey = (usize, Vec<(String, String)>);

fn key(rule: usize, wildcards: &Wildcards) -> JobKey {
    let mut pairs: Vec<(String, String)> = wildcards
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect();
    pairs.sort();
    (rule, pairs)
}

pub struct Planner {
    pub rules: Vec<Rule>,
    pub ruleorder: Vec<Vec<String>>,
    pub jobs: Vec<Job>,
    index: HashMap<JobKey, usize>,
    /// Jobs whose task was asked for; they keep their plan when the workflow is planned again.
    pub fixed: HashSet<usize>,
    producers: HashMap<String, Option<usize>>,
    failed: HashMap<JobKey, Failure>,
    stack: Vec<(usize, Wildcards)>,
    matchers: Vec<Vec<Rc<Matcher>>>,
    pub assume_inputs: bool,
}

impl Planner {
    pub fn new(rules: Vec<Rule>, ruleorder: Vec<Vec<String>>, assume_inputs: bool) -> Planner {
        let matchers = rules
            .iter()
            .map(|rule| {
                rule.products()
                    .iter()
                    .map(|product| Matcher::of(product))
                    .collect()
            })
            .collect();
        Planner {
            rules,
            ruleorder,
            jobs: Vec::new(),
            index: HashMap::new(),
            fixed: HashSet::new(),
            producers: HashMap::new(),
            failed: HashMap::new(),
            stack: Vec::new(),
            matchers,
            assume_inputs,
        }
    }

    /// Forgets the plan except the jobs that have tasks, before planning again.
    pub fn reset(&mut self) {
        self.index.retain(|_, job| self.fixed.contains(job));
        self.producers.clear();
        self.failed.clear();
        self.stack.clear();
    }

    /// The jobs that the roots need, each after the jobs it depends on.
    pub fn ordered_jobs(&self, roots: &[usize]) -> Vec<usize> {
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        let mut stack: Vec<(usize, bool)> = roots.iter().rev().map(|root| (*root, false)).collect();
        while let Some((job, expanded)) = stack.pop() {
            if expanded {
                order.push(job);
                continue;
            }
            if !seen.insert(job) {
                continue;
            }
            stack.push((job, true));
            for (_, dependency) in self.jobs[job].dependencies.iter().rev() {
                if !seen.contains(dependency) {
                    stack.push((*dependency, false));
                }
            }
        }
        order
    }

    /// The input files that the jobs of the roots assume exist.
    pub fn assumed_files(&self, roots: &[usize]) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for job in self.ordered_jobs(roots) {
            for file in &self.jobs[job].missing {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
        }
        files
    }

    fn exists(&self, interpreter: &Interpreter, file: &str) -> bool {
        let path = interpreter.host.resolve(file);
        interpreter.host.stat(&path).is_some()
    }

    /// Snakemake's `Ruleorder.compare`: positive when `b` has priority over `a`.
    fn compare(&self, a: usize, b: usize) -> i32 {
        let (name_a, name_b) = (&self.rules[a].name, &self.rules[b].name);
        if name_a != name_b {
            for clause in self.ruleorder.iter().rev() {
                let (Some(i), Some(j)) = (
                    clause.iter().position(|name| name == name_a),
                    clause.iter().position(|name| name == name_b),
                ) else {
                    continue;
                };
                return (j as i32 - i as i32).signum();
            }
        }
        // Without an order, a rule without wildcards comes first.
        i32::from(self.rules[b].has_wildcards()) - i32::from(self.rules[a].has_wildcards())
    }

    /// The wildcard values of a rule for a file it makes: first those of the job that asks
    /// for the file, then the match with the shortest values, as `Rule.get_wildcards` does.
    fn wildcards_for(
        &self,
        rule: usize,
        file: &str,
        consumer: Option<&Wildcards>,
    ) -> Option<Wildcards> {
        let names = self.rules[rule].wildcard_names();
        if let Some(consumer) = consumer
            && names.iter().all(|name| consumer.contains_key(name))
        {
            let given: Wildcards = names
                .iter()
                .map(|name| (name.clone(), consumer[name].clone()))
                .collect();
            for product in self.rules[rule].products() {
                if pattern::apply(&product, &given).ok().as_deref() == Some(file)
                    && pattern::satisfies_constraints(&product, &given)
                {
                    return Some(given);
                }
            }
        }
        let mut best: Option<(usize, Wildcards)> = None;
        for matcher in &self.matchers[rule] {
            if let Some(values) = matcher.matches(file) {
                let length: usize = values.values().map(String::len).sum();
                if best
                    .as_ref()
                    .is_none_or(|(best_length, _)| length < *best_length)
                {
                    best = Some((length, values));
                }
            }
        }
        best.map(|(_, values)| values)
    }

    /// The rules whose outputs, logs or benchmarks match a file.
    fn candidates(&self, file: &str) -> Vec<usize> {
        (0..self.rules.len())
            .filter(|rule| {
                self.matchers[*rule]
                    .iter()
                    .any(|matcher| matcher.matches(file).is_some())
            })
            .collect()
    }

    /// Whether a job would repeat a rule of the jobs that ask for it with growing wildcard
    /// values, like `{x}` from `{x}.gz` from `{x}.gz.gz`, or with values that repeat.
    fn pumps(&self, rule: usize, wildcards: &Wildcards) -> bool {
        if wildcards
            .values()
            .any(|value| pattern::is_periodic(value, 20))
        {
            return true;
        }
        self.stack.iter().any(|(other, values)| {
            *other == rule
                && wildcards.iter().any(|(name, value)| {
                    values.get(name).is_some_and(|previous| {
                        value != previous && value.contains(previous.as_str())
                    })
                })
        })
    }

    /// The job that makes a file, `None` for a file that no rule makes.
    pub fn plan_file(
        &mut self,
        interpreter: &mut Interpreter,
        file: &str,
        consumer: Option<&Wildcards>,
    ) -> Result<Option<usize>, Failure> {
        if let Some(found) = self.producers.get(file) {
            return Ok(*found);
        }
        let mut candidates = self.candidates(file);
        if candidates.is_empty() {
            self.producers.insert(file.to_owned(), None);
            return Ok(None);
        }
        // Higher priority first; the sort is stable, so equal rules keep their order.
        candidates.sort_by(|a, b| self.compare(*a, *b).cmp(&0).reverse());
        // Each producer with the length of its wildcard values.
        let mut producers: Vec<(usize, usize, usize)> = Vec::new();
        let mut failures: Vec<Failure> = Vec::new();
        let mut discarded: Vec<usize> = Vec::new();
        for rule in candidates.iter().copied() {
            let Some(wildcards) = self.wildcards_for(rule, file, consumer) else {
                discarded.push(rule);
                continue;
            };
            if self
                .stack
                .iter()
                .any(|(other, values)| *other == rule && *values == wildcards)
                || self.pumps(rule, &wildcards)
            {
                failures.push(Failure::Cycle(format!(
                    "Cyclic dependency on rule {} for file {file}",
                    self.rules[rule].name
                )));
                discarded.push(rule);
                continue;
            }
            let length = wildcards.values().map(String::len).sum();
            match self.plan_job(interpreter, rule, wildcards, Some(file)) {
                Ok(job) => {
                    producers.push((rule, job, length));
                    let strictly_higher = candidates
                        .iter()
                        .filter(|other| **other != rule && !discarded.contains(other))
                        .all(|other| self.compare(rule, *other) > 0);
                    if strictly_higher && !self.jobs[job].assumed {
                        break;
                    }
                }
                Err(failure) => {
                    failures.push(failure);
                    discarded.push(rule);
                }
            }
        }
        if producers.is_empty() {
            let only_cycles = failures
                .iter()
                .all(|failure| matches!(failure, Failure::Cycle(_)));
            if self.exists(interpreter, file) || (self.assume_inputs && only_cycles) {
                self.producers.insert(file.to_owned(), None);
                return Ok(None);
            }
            return Err(failures
                .into_iter()
                .next()
                .unwrap_or(Failure::Cycle(format!("No producer for {file}"))));
        }
        // Jobs whose inputs all exist win over jobs that need assumed inputs, then the rule
        // order decides. Among jobs that need assumed inputs, which Snakemake would not
        // consider, the one with the shortest wildcard values wins.
        let exact = producers.iter().any(|(_, job, _)| !self.jobs[*job].assumed);
        let mut viable: Vec<(usize, usize, usize)> = producers
            .into_iter()
            .filter(|(_, job, _)| !exact || !self.jobs[*job].assumed)
            .collect();
        viable.sort_by(|(a, _, length_a), (b, _, length_b)| {
            let order = self.compare(*a, *b).cmp(&0).reverse();
            if exact {
                order
            } else {
                order.then(length_a.cmp(length_b))
            }
        });
        let (rule, job, _) = viable[0];
        if exact
            && let Some((other, _, _)) = viable[1..]
                .iter()
                .find(|(other, _, _)| self.compare(rule, *other) == 0)
        {
            return Err(Failure::Error(crate::workflow::workflow_error(format!(
                "Rules {} and {} are ambiguous for the file {file}. Consider starting rule output with a unique prefix, constrain your wildcards, or use the ruleorder directive.",
                self.rules[rule].name, self.rules[*other].name
            ))));
        }
        self.producers.insert(file.to_owned(), Some(job));
        Ok(Some(job))
    }

    /// Plans the job of a rule with wildcard values, with the jobs that make its inputs.
    pub fn plan_job(
        &mut self,
        interpreter: &mut Interpreter,
        rule: usize,
        wildcards: Wildcards,
        target: Option<&str>,
    ) -> Result<usize, Failure> {
        let job_key = key(rule, &wildcards);
        if let Some(job) = self.index.get(&job_key) {
            return Ok(*job);
        }
        if let Some(failure) = self.failed.get(&job_key) {
            return Err(failure.clone());
        }
        self.stack.push((rule, wildcards.clone()));
        let result = self.plan_job_inner(interpreter, rule, &wildcards, target);
        self.stack.pop();
        match result {
            Ok(job) => {
                let id = self.jobs.len();
                self.jobs.push(job);
                self.index.insert(job_key, id);
                Ok(id)
            }
            Err(failure) => {
                if matches!(failure, Failure::Error(_)) {
                    self.failed.insert(job_key, failure.clone());
                }
                Err(failure)
            }
        }
    }
}
