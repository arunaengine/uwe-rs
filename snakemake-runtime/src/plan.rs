//! Plans jobs backwards from target files; simulated runs may assume unavailable raw inputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
    /// The first outputs of the checkpoints that the input functions consulted.
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

    /// The outputs marked `touch()`, which Snakemake makes itself once the job has run.
    pub fn touched_files(&self) -> Vec<String> {
        (self.output.values.iter())
            .filter(|value| value.tag("touch").is_some())
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect()
    }

    /// The outputs marked `temp()`, which Snakemake removes once no job needs them, so that a
    /// run does not deliver them; `pipe()` and `service()` outputs are temporary too.
    pub fn temporary_files(&self) -> Vec<String> {
        (self.output.values.iter())
            .filter(|value| {
                ["temp", "pipe", "service"]
                    .iter()
                    .any(|flag| value.tag(flag).is_some())
            })
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect()
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
    /// Outputs of checkpoints that completed successfully in this run.
    pub checkpoints_done: HashSet<String>,
    checkpoints_ready: Rc<RefCell<HashSet<String>>>,
    producers: HashMap<String, Option<usize>>,
    failed: HashMap<JobKey, Failure>,
    stack: Vec<(usize, Wildcards)>,
    matchers: Vec<Vec<Rc<Matcher>>>,
    pub assume_inputs: bool,
    pub forcerun: Vec<String>,
    pub forceall: bool,
    pub mtime: bool,
    pub code: bool,
    /// The directory outputs that jobs of this run made, by their absolute paths.
    pub made_folders: Vec<PathBuf>,
    /// The rules of `allowed-rules`, the only ones that make files when given.
    pub allowed: Option<Vec<String>>,
    /// The `batch` of a profile: the rule, the batch and the number of batches.
    pub batch: Option<(String, usize, usize)>,
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
            checkpoints_done: HashSet::new(),
            checkpoints_ready: Rc::default(),
            producers: HashMap::new(),
            failed: HashMap::new(),
            stack: Vec::new(),
            matchers,
            assume_inputs,
            forcerun: Vec::new(),
            forceall: false,
            mtime: true,
            code: true,
            made_folders: Vec::new(),
            allowed: None,
            batch: None,
        }
    }

    /// Forgets the plan except the jobs that have tasks, before planning again.
    pub fn reset(&mut self) {
        self.index.retain(|_, job| self.fixed.contains(job));
        self.producers.clear();
        self.failed.clear();
        self.stack.clear();
        *self.checkpoints_ready.borrow_mut() = self.checkpoints_done.clone();
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

    fn matches_targets(&self, job: usize, targets: &[String]) -> bool {
        let entry = &self.jobs[job];
        targets.iter().any(
            |target| match self.rules.iter().position(|rule| rule.name == *target) {
                Some(rule) => entry.rule == rule,
                None => entry
                    .output
                    .values
                    .iter()
                    .any(|value| value.as_str() == Some(target)),
            },
        )
    }

    /// Snakemake 9 adds concrete until/force targets before selecting until jobs and
    /// deleting omit-from jobs. Wildcard rule selectors match jobs already in the DAG.
    pub fn plan_targets(
        &mut self,
        interpreter: &mut Interpreter,
        targets: &[String],
        until: &[String],
        omit_from: &[String],
    ) -> Result<Vec<usize>, Error> {
        let mut roots = Vec::new();
        let extras: Vec<String> = until.iter().chain(&self.forcerun).cloned().collect();
        for (target, extra) in targets
            .iter()
            .map(|target| (target, false))
            .chain(extras.iter().map(|target| (target, true)))
        {
            let rule = self.rules.iter().find(|rule| rule.name == *target);
            if extra && rule.is_some_and(Rule::has_wildcards) {
                continue;
            }
            let root = if rule.is_some() {
                Some(self.plan_rule(interpreter, target)?)
            } else {
                self.plan_target_file(interpreter, target)?
            };
            if let Some(root) = root {
                roots.push(root);
            }
        }
        if !until.is_empty() {
            roots = self
                .ordered_jobs(&roots)
                .into_iter()
                .filter(|job| self.matches_targets(*job, until))
                .collect();
        }
        if !omit_from.is_empty() {
            let mut omitted = HashSet::new();
            let order = self.ordered_jobs(&roots);
            for job in &order {
                if self.matches_targets(*job, omit_from)
                    || self.jobs[*job]
                        .dependencies
                        .iter()
                        .any(|(_, dependency)| omitted.contains(dependency))
                {
                    omitted.insert(*job);
                }
            }
            // Dependencies of deleted jobs remain targets, including independent siblings.
            roots = order
                .into_iter()
                .filter(|job| !omitted.contains(job))
                .collect();
        }
        Ok(roots)
    }

    /// Forced jobs and their downstream jobs must bypass task caches and resume history.
    pub fn forced_jobs(&self, roots: &[usize]) -> HashSet<usize> {
        let mut forced = HashSet::new();
        for job in self.ordered_jobs(roots) {
            if self.forceall
                || self.matches_targets(job, &self.forcerun)
                || self.jobs[job]
                    .dependencies
                    .iter()
                    .any(|(_, dependency)| forced.contains(dependency))
            {
                forced.insert(job);
            }
        }
        forced
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

    /// The input files that the jobs planned so far assumed to exist.
    pub fn all_assumed_files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for job in &self.jobs {
            for file in &job.missing {
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

    /// The rules whose outputs, logs or benchmarks match a file. A constraint that is no
    /// valid regex fails once a rule may make the file, as `Rule.is_producer` does.
    fn candidates(&self, file: &str) -> Result<Vec<usize>, Failure> {
        let mut found = Vec::new();
        for (rule, matchers) in self.matchers.iter().enumerate() {
            if !matchers.iter().any(|matcher| matcher.may_match(file)) || !self.listed(rule) {
                continue;
            }
            for matcher in matchers {
                if let Some(error) = matcher.error() {
                    let rule = &self.rules[rule];
                    return Err(Failure::Error(crate::workflow::workflow_error(format!(
                        "IOFileException in file \"{}\", line {}:\n{error} in wildcard statement",
                        rule.snakefile, rule.line
                    ))));
                }
                if matcher.matches(file).is_some() {
                    found.push(rule);
                    break;
                }
            }
        }
        Ok(found)
    }

    /// Whether `allowed-rules` lets a rule make the files of other jobs. With checkpoints,
    /// Snakemake plans with every rule but runs only the allowed ones, as `runs` tells.
    fn listed(&self, rule: usize) -> bool {
        self.allows(rule, false)
    }

    fn runs(&self, rule: usize) -> bool {
        self.allows(rule, true)
    }

    fn allows(&self, rule: usize, running: bool) -> bool {
        let checkpoints = self.rules.iter().any(|rule| rule.checkpoint);
        let name = &self.rules[rule].name;
        (self.allowed.as_ref())
            .is_none_or(|allowed| checkpoints != running || allowed.contains(name))
    }

    /// Whether wildcard values contain a repeated suffix from unbounded dependency growth.
    fn pumps(wildcards: &Wildcards) -> bool {
        wildcards
            .values()
            .any(|value| pattern::is_periodic(value, 20))
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
        // A file that a directory output of a finished job holds comes from that job, as in
        // Snakemake, which plans no other job for it (test_checkpoints_many).
        let path = interpreter.host.resolve(file);
        let made =
            (self.made_folders.iter()).any(|folder| path.starts_with(folder) && path != *folder);
        if made && self.exists(interpreter, file) {
            self.producers.insert(file.to_owned(), None);
            return Ok(None);
        }
        let mut candidates = self.candidates(file)?;
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
                || Self::pumps(&wildcards)
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
        if !exact && self.exists(interpreter, file) {
            self.producers.insert(file.to_owned(), None);
            return Ok(None);
        }
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

    fn plan_job_inner(
        &mut self,
        interpreter: &mut Interpreter,
        rule_index: usize,
        wildcards: &Wildcards,
        target: Option<&str>,
    ) -> Result<Job, Failure> {
        let rule = self.rules[rule_index].clone();
        let mut checkpoint_dependencies = Vec::new();
        let (input, incomplete, checkpoints) = loop {
            crate::objects::set_checkpoint_ready(Some(&self.checkpoints_ready));
            crate::objects::take_consulted();
            let expanded = expand_input(interpreter, &rule, wildcards);
            crate::objects::set_checkpoint_ready(None);
            let (input, incomplete) = expanded.map_err(Failure::Error)?;
            let checkpoints = crate::objects::take_consulted();
            let mut newly_ready = false;
            for (file, name) in &checkpoints {
                let rule = self.rules.iter().position(|rule| rule.name == *name);
                let producer = match rule {
                    Some(rule) => match self.wildcards_for(rule, file, Some(wildcards)) {
                        // A checkpoint whose output is there runs not again, so its missing
                        // inputs do not matter, as in Snakemake (test_uncreatable_checkpoint_input).
                        Some(values) => {
                            match self.plan_job(interpreter, rule, values, Some(file)) {
                                Ok(job) => Some(job),
                                Err(_) if self.exists(interpreter, file) => None,
                                Err(failure) => return Err(failure),
                            }
                        }
                        None => self.plan_file(interpreter, file, Some(wildcards))?,
                    },
                    None => self.plan_file(interpreter, file, Some(wildcards))?,
                };
                if producer.is_none()
                    && self.exists(interpreter, file)
                    && self.checkpoints_ready.borrow_mut().insert(file.clone())
                {
                    newly_ready = true;
                }
                if let Some(producer) = producer {
                    if !checkpoint_dependencies
                        .iter()
                        .any(|known| known == &(file.clone(), producer))
                    {
                        checkpoint_dependencies.push((file.clone(), producer));
                    }
                    if !self.checkpoints_ready.borrow().contains(file)
                        && !self
                            .needrun(interpreter, &[producer])
                            .get(&producer)
                            .copied()
                            .unwrap_or(true)
                    {
                        self.checkpoints_ready.borrow_mut().insert(file.clone());
                        newly_ready = true;
                    }
                }
            }
            if !newly_ready {
                break (
                    input,
                    incomplete,
                    checkpoints.into_iter().map(|(file, _)| file).collect(),
                );
            }
        };
        let fill = |items: &Items, what: &str| -> Result<Items, Failure> {
            let mut filled = items.clone();
            filled.values = items
                .values
                .iter()
                .map(|value| match value.as_str() {
                    Some(text) => pattern::apply(text, wildcards).and_then(|text| {
                        let mut filled = retag(value, Value::str(text));
                        if let Some(address) = value.tag(crate::storage::TAG).and_then(Value::as_str) {
                            filled = filled.with_tag(crate::storage::TAG, Value::str(pattern::apply(address, wildcards)?));
                        }
                        Ok(filled)
                    }).map_err(|name| {
                        Failure::Error(crate::workflow::workflow_error(format!(
                            "Wildcards in {what} files cannot be determined from output files: '{name}'"
                        )))
                    }),
                    None => Ok(value.clone()),
                })
                .collect::<Result<_, _>>()?;
            Ok(filled)
        };
        let output = fill(&rule.output, "output")?;
        for value in input.values.iter().chain(&output.values) {
            if let Some(address) = value.tag(crate::storage::TAG).and_then(Value::as_str)
                && let Some(file) = value.as_str()
            {
                interpreter
                    .host
                    .map_storage(&interpreter.host.resolve(file), address)
                    .map_err(|error| {
                        Failure::Error(crate::workflow::workflow_error(error.to_string()))
                    })?;
            }
        }
        let log = fill(&rule.log, "log")?;
        let benchmark = match &rule.benchmark {
            Some(benchmark) => match benchmark.as_str() {
                Some(text) => Some(Value::str(pattern::apply(text, wildcards).map_err(|name| {
                    Failure::Error(crate::workflow::workflow_error(format!(
                        "Wildcards in benchmark file cannot be determined from output files: '{name}'"
                    )))
                })?)),
                None => Some(benchmark.clone()),
            },
            None => None,
        };
        let mut job = Job {
            rule: rule_index,
            wildcards: wildcards.clone(),
            input,
            output,
            log,
            benchmark,
            dependencies: checkpoint_dependencies,
            incomplete,
            assumed: false,
            missing: Vec::new(),
            checkpoints,
        };
        if let Some(target) = target
            && job.input_files().iter().any(|file| file == target)
        {
            return Err(Failure::Cycle(format!(
                "Rule {} consumes its own output {target}",
                rule.name
            )));
        }
        // Each input with the rule that its value names, whether it is in storage, and whether
        // the job reads it as it was before an update (`before_update`).
        let inputs: Vec<(String, Option<String>, bool, bool)> = {
            let mut seen: Vec<String> = Vec::new();
            let mut inputs = Vec::new();
            for value in &job.input.values {
                let Some(text) = value.as_str() else { continue };
                if seen.iter().any(|file| file == text) {
                    continue;
                }
                seen.push(text.to_owned());
                let dependency = value
                    .tag("rule")
                    .and_then(|rule| rule.as_str().map(str::to_owned));
                let stored = value.tag(crate::storage::TAG).is_some();
                let before = value.tag("before_update").is_some();
                inputs.push((text.to_owned(), dependency, stored, before));
            }
            inputs
        };
        let inputs = match &self.batch {
            Some(batch) if batch.0 == rule.name => self.batched(interpreter, batch, inputs)?,
            _ => inputs,
        };
        for (file, dependency, stored, before) in inputs {
            // Snakemake plans no job for a file that the job reads as it was before an update.
            if before {
                if !self.exists(interpreter, &file) && !self.assume_inputs {
                    return Err(Failure::Error(crate::workflow::workflow_error(format!(
                        "Missing input files for rule {}:\n    affected files:\n        {file}",
                        rule.name
                    ))));
                }
                continue;
            }
            let planned = match dependency
                .and_then(|name| self.rules.iter().position(|rule| rule.name == name))
            {
                Some(producer) => match self.wildcards_for(producer, &file, Some(wildcards)) {
                    Some(values) => self
                        .plan_job(interpreter, producer, values, Some(&file))
                        .map(Some),
                    None => self.plan_file(interpreter, &file, Some(wildcards)),
                },
                None => self.plan_file(interpreter, &file, Some(wildcards)),
            };
            match planned {
                Ok(Some(producer)) => {
                    job.assumed |= self.jobs[producer].assumed;
                    job.dependencies.push((file, producer));
                }
                Ok(None) if stored && self.assume_inputs => {}
                Ok(None) => {
                    if !self.exists(interpreter, &file) {
                        if !self.assume_inputs {
                            return Err(Failure::Error(crate::workflow::workflow_error(format!(
                                "Missing input files for rule {}:\n    affected files:\n        {file}",
                                rule.name
                            ))));
                        }
                        job.assumed = true;
                        job.missing.push(file);
                    }
                }
                Err(failure) => {
                    if (stored && self.assume_inputs) || self.exists(interpreter, &file) {
                        continue;
                    }
                    return Err(failure);
                }
            }
        }
        Ok(job)
    }

    /// The inputs of the batch of the `batch` rule, as Snakemake's `Batch.get_batch`: the
    /// sorted inputs split in equal parts. The final batch needs the others to be there.
    fn batched<T>(
        &self,
        interpreter: &Interpreter,
        (rule, number, total): &(String, usize, usize),
        inputs: Vec<(String, T, bool, bool)>,
    ) -> Result<Vec<(String, T, bool, bool)>, Failure> {
        let error = |message: String| Failure::Error(crate::workflow::workflow_error(message));
        if inputs.len() < *total {
            return Err(error(format!(
                "Batching rule {rule} has less input files than batches. Please choose a smaller number of batches."
            )));
        }
        let mut files: Vec<&str> = inputs.iter().map(|(file, ..)| file.as_str()).collect();
        files.sort_unstable();
        let (size, rest) = (files.len() / total, files.len() % total);
        let index = number - 1;
        let start = index * size + index.min(rest);
        let end = match number == total {
            true => files.len(),
            false => (index + 1) * size + (index + 1).min(rest),
        };
        let batch: Vec<String> = files[start..end]
            .iter()
            .map(|file| file.to_string())
            .collect();
        let earlier = files
            .iter()
            .filter(|file| !batch.iter().any(|kept| kept == *file));
        if number == total && earlier.clone().any(|file| !self.exists(interpreter, file)) {
            return Err(error(format!(
                "Unable to execute batch {number}/{total} (rule {rule}) because not all previous batches have been completed before or files have been deleted."
            )));
        }
        Ok(inputs
            .into_iter()
            .filter(|(file, ..)| batch.contains(file))
            .collect())
    }

    /// Plans a target rule, whose job has no wildcards.
    pub fn plan_rule(&mut self, interpreter: &mut Interpreter, name: &str) -> Result<usize, Error> {
        let Some(rule) = self.rules.iter().position(|rule| rule.name == name) else {
            return Err(crate::workflow::workflow_error(format!(
                "Rule {name} is not defined in this workflow."
            )));
        };
        if self.rules[rule].has_wildcards() {
            return Err(crate::workflow::workflow_error(
                "Target rules may not contain wildcards. Please specify concrete files or a rule without wildcards at the command line, or have a rule without wildcards at the very top of your workflow (e.g. the typical \"rule all\" which just collects all results you want to generate in the end).",
            ));
        }
        self.plan_job(interpreter, rule, Wildcards::new(), None)
            .map_err(Failure::into_error)
    }

    /// Plans a target file.
    pub fn plan_target_file(
        &mut self,
        interpreter: &mut Interpreter,
        file: &str,
    ) -> Result<Option<usize>, Error> {
        match self
            .plan_file(interpreter, file, None)
            .map_err(Failure::into_error)?
        {
            Some(job) => Ok(Some(job)),
            None if self.exists(interpreter, file) => Ok(None),
            None => Err(crate::workflow::workflow_error(format!(
                "MissingRuleException: No rule to produce {file}"
            ))),
        }
    }

    /// Whether each job must run: an output is missing, the rule has no output, it waits
    /// for a checkpoint, or a job it depends on must run.
    pub fn needrun(&self, interpreter: &Interpreter, roots: &[usize]) -> HashMap<usize, bool> {
        let mut result: HashMap<usize, bool> = HashMap::new();
        for root in roots {
            self.visit_needrun(interpreter, *root, roots, &mut result, &mut HashSet::new());
        }
        let mut consumers: HashMap<usize, Vec<(usize, &str)>> = HashMap::new();
        for job in self.ordered_jobs(roots) {
            for (file, dependency) in &self.jobs[job].dependencies {
                consumers.entry(*dependency).or_default().push((job, file));
            }
        }
        let mut pending: Vec<usize> = result
            .iter()
            .filter_map(|(job, needed)| needed.then_some(*job))
            .collect();
        while let Some(job) = pending.pop() {
            for (file, dependency) in &self.jobs[job].dependencies {
                let allowed = self.runs(self.jobs[*dependency].rule);
                if !self.exists(interpreter, file) && !result[dependency] && allowed {
                    result.insert(*dependency, true);
                    pending.push(*dependency);
                }
            }
            for (consumer, file) in consumers.get(&job).into_iter().flatten() {
                let ancient =
                    self.jobs[*consumer].input.values.iter().any(|value| {
                        value.as_str() == Some(*file) && value.tag("ancient").is_some()
                    });
                let allowed = self.runs(self.jobs[*consumer].rule);
                if !result[consumer] && !(ancient && self.exists(interpreter, file)) && allowed {
                    result.insert(*consumer, true);
                    pending.push(*consumer);
                }
            }
        }
        result
    }

    fn visit_needrun(
        &self,
        interpreter: &Interpreter,
        job: usize,
        roots: &[usize],
        result: &mut HashMap<usize, bool>,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        if let Some(found) = result.get(&job) {
            return *found;
        }
        if !visiting.insert(job) {
            return false;
        }
        let mut needed = self.forceall || self.matches_targets(job, &self.forcerun);
        for (file, dependency) in &self.jobs[job].dependencies {
            let upstream = self.visit_needrun(interpreter, *dependency, roots, result, visiting);
            let ancient = self.jobs[job]
                .input
                .values
                .iter()
                .any(|value| value.as_str() == Some(file) && value.tag("ancient").is_some());
            needed |= (upstream || self.fixed.contains(dependency))
                && !(ancient && self.exists(interpreter, file));
        }
        let outputs = self.jobs[job].output_files();
        let own_outputs: Vec<String> = self.jobs[job]
            .output
            .values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        let target = roots.contains(&job);
        let no_output = own_outputs.is_empty()
            && target
            && (!matches!(
                self.rules[self.jobs[job].rule].action,
                crate::rule::Action::None
            ) || self.jobs[job].input.values.is_empty()
                || self.jobs[job]
                    .input_files()
                    .iter()
                    .any(|file| !self.exists(interpreter, file)));
        needed |= no_output
            || target && self.jobs[job].incomplete
            || target
                && outputs
                    .iter()
                    .filter(|file| own_outputs.contains(file))
                    .any(|file| !self.exists(interpreter, file));
        needed = needed || (self.mtime && self.has_newer_inputs(interpreter, job, &own_outputs));
        needed = needed || (self.code && self.has_newer_code(interpreter, job, &own_outputs));
        needed &= self.runs(self.jobs[job].rule);
        result.insert(job, needed);
        needed
    }

    fn has_newer_code(&self, interpreter: &Interpreter, job: usize, outputs: &[String]) -> bool {
        let rule = &self.rules[self.jobs[job].rule];
        let script = match &rule.action {
            crate::rule::Action::Script(script) | crate::rule::Action::Notebook(script) => script,
            _ => return false,
        };
        let Some(script) = script.as_str().filter(|script| !script.contains("://")) else {
            return false;
        };
        let Ok(script) = pattern::apply(script, &self.jobs[job].wildcards) else {
            return false;
        };
        let path = interpreter
            .host
            .resolve(&rule.basedir.join(script).to_string_lossy());
        let Some(script) = interpreter.host.stat(&path) else {
            return false;
        };
        outputs.iter().any(|output| {
            let path = interpreter.host.resolve(output);
            interpreter
                .host
                .stat(&path)
                .is_some_and(|output| output.modified <= script.modified)
        })
    }

    /// Snakemake's `mtime` trigger: a job reruns when a local input that is not `ancient` is
    /// newer than its oldest output and its checksum is not recorded. A linked output counts
    /// by the time of the link, a linked input by the later of that and its target's time.
    fn has_newer_inputs(&self, interpreter: &Interpreter, job: usize, outputs: &[String]) -> bool {
        let modified = |file: &str, input: bool| {
            let path = interpreter.host.resolve(file);
            let followed = interpreter.host.stat(&path).map(|stat| stat.modified);
            let link = (interpreter.host.is_symlink(&path))
                .then(|| std::fs::symlink_metadata(&path).ok()?.modified().ok())
                .flatten()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|time| time.as_secs_f64());
            let time = match (link, followed) {
                (Some(link), Some(followed)) if input => Some(link.max(followed)),
                (Some(link), _) => Some(link),
                (None, followed) => followed,
            };
            time.filter(|modified| *modified > 0.0)
        };
        let Some(oldest) = outputs
            .iter()
            .filter_map(|file| modified(file, false))
            .reduce(f64::min)
        else {
            return false;
        };
        (self.jobs[job].input.values.iter())
            .filter(|value| value.tag("ancient").is_none())
            .filter_map(|value| value.as_str())
            .filter(|file| modified(file, true).is_some_and(|time| time > oldest))
            .any(|file| !crate::metadata::same_checksum(interpreter, outputs, file))
    }
}

/// The group id that input functions with a `groupid` parameter get. Snakemake gives every
/// job `local` in a local run, where it forms no group jobs; uwe runs each job as a task
/// of its own, so every job gets `local` too (Snakemake 9.27, test_groupid_expand).
const GROUP_ID: &str = "local";

/// The input files of a job: patterns with wildcards filled in, and what input functions
/// return. Also whether a function waits for a checkpoint, whose output then stands in.
pub fn expand_input(
    interpreter: &mut Interpreter,
    rule: &Rule,
    wildcards: &Wildcards,
) -> Result<(Items, bool), Error> {
    let wildcards_value = NamedList::wildcards(wildcards);
    let mut items = Items::default();
    let mut incomplete = false;
    for (name, values, many) in rule.input.named_entries() {
        let start = items.values.len();
        for value in values {
            if is_callable(interpreter, &value) {
                let unpack = value.tag("unpack").is_some();
                let aux = [("groupid", Value::str(GROUP_ID))];
                let result =
                    match call_input_function(interpreter, value.plain(), &wildcards_value, &aux) {
                        Ok(result) => result,
                        Err(error) => match incomplete_target(&error) {
                            Some(target) => {
                                incomplete = true;
                                target
                            }
                            None => {
                                return Err(input_function_error(error, rule, wildcards));
                            }
                        },
                    };
                if unpack && !incomplete {
                    match result.plain() {
                        Value::Dict(_) => {
                            for (key, item) in interpreter.mapping_items(&result)? {
                                let key = interpreter.to_str(&key)?;
                                let item_start = items.values.len();
                                let many = add_function_result(
                                    interpreter,
                                    &mut items,
                                    &item,
                                    &value,
                                    wildcards,
                                    &rule.pathvars,
                                )?;
                                items.set_name(
                                    &key,
                                    item_start,
                                    many.then_some(items.values.len()),
                                );
                            }
                        }
                        Value::List(_) | Value::Tuple(_) => {
                            for item in interpreter.iterate(&result)? {
                                add_function_result(
                                    interpreter,
                                    &mut items,
                                    &item,
                                    &value,
                                    wildcards,
                                    &rule.pathvars,
                                )?;
                            }
                        }
                        _ => {
                            return Err(crate::workflow::workflow_error(format!(
                                "Can only use unpack() on list and dict, but {} was returned.",
                                interpreter.repr(&result)?
                            )));
                        }
                    }
                    continue;
                }
                let many = add_function_result(
                    interpreter,
                    &mut items,
                    &result,
                    &value,
                    wildcards,
                    &rule.pathvars,
                )?;
                if let Some(name) = &name {
                    items.set_name(name, start, many.then_some(items.values.len()));
                }
            } else {
                let text = interpreter.to_str(&value)?;
                let filled = pattern::apply(&text, wildcards).map_err(|missing| {
                    crate::workflow::workflow_error(format!(
                        "Wildcards in input files cannot be determined from output files: '{missing}'"
                    ))
                })?;
                let mut filled = retag(&value, Value::str(filled));
                // The address of a file in storage has the same wildcards.
                if let Some(query) = value.tag(crate::storage::TAG).and_then(|query| {
                    query
                        .as_str()
                        .and_then(|query| pattern::apply(query, wildcards).ok())
                }) {
                    filled = filled.with_tag(crate::storage::TAG, Value::str(query));
                }
                items.push(filled);
            }
        }
        if let Some(name) = &name
            && !items.names.iter().any(|(existing, _, _)| existing == name)
        {
            items.set_name(name, start, many.then_some(items.values.len()));
        }
    }
    Ok((items, incomplete))
}

/// Adds what an input function returned: a file, or a list of files. Returns whether it was
/// a list.
fn add_function_result(
    interpreter: &mut Interpreter,
    items: &mut Items,
    result: &Value,
    function: &Value,
    wildcards: &Wildcards,
    pathvars: &crate::pathvars::Pathvars,
) -> Result<bool, Error> {
    let files = match result.plain() {
        Value::Str(_) => vec![result.clone()],
        Value::Native(object) if object.type_name() == "PosixPath" => {
            vec![Value::str(interpreter.to_str(result)?)]
        }
        Value::List(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Iterator(_)
        | Value::Native(_) => interpreter.iterate(result)?,
        other => {
            return Err(crate::workflow::workflow_error(format!(
                "Function did not return str or iterable of str. Encountered: {} ({})",
                interpreter.repr(other)?,
                python_evaluator::types::type_name(other)
            )));
        }
    };
    let many = !matches!(result.plain(), Value::Str(_))
        && result
            .plain()
            .as_native::<python_evaluator::modules::PathObject>()
            .is_none();
    for file in files {
        let text = match file.plain() {
            Value::Str(text) => text.to_string(),
            Value::Native(object) if object.type_name() == "PosixPath" => {
                interpreter.to_str(&file)?
            }
            other => {
                return Err(crate::workflow::workflow_error(format!(
                    "Function did not return str or iterable of str. Encountered: {} ({})",
                    interpreter.repr(result)?,
                    python_evaluator::types::type_name(other)
                )));
            }
        };
        let text = pathvars.apply(text.trim_end_matches('/'))?;
        let filled = pattern::apply(&text, wildcards).unwrap_or(text);
        let mut value = match crate::workflow::module_path(function, &filled) {
            Some(modified) if modified != filled && file.tag("path_modified").is_none() => {
                retag(&file, Value::str(modified)).with_tag("path_modified", Value::Bool(true))
            }
            _ => retag(&file, Value::str(filled)),
        };
        if let Some(tags) = function.tags() {
            for (flag, flag_value) in tags {
                let own = ["unpack", crate::workflow::MODULE_PATHS].contains(&&**flag);
                if !own && value.tag(flag).is_none() {
                    value = value.with_tag(flag, flag_value.clone());
                }
            }
        }
        if let Some(Value::Tuple(defaults)) = value.tag("storage_default").map(Value::plain)
            && let [provider, prefix] = defaults.as_ref()
            && let (Some(provider), Some(prefix)) = (provider.as_str(), prefix.as_str())
        {
            value = crate::storage::default_file(interpreter, &value, provider, prefix)?;
        }
        items.push(value);
    }
    Ok(many)
}

/// The output file of a checkpoint that an input function waits for.
pub fn incomplete_target(error: &Error) -> Option<Value> {
    let ErrorKind::Exception(exception) = &error.kind else {
        return None;
    };
    if !python_evaluator::types::class_of(exception).derives_from("IncompleteCheckpointException") {
        return None;
    }
    let Value::Instance(instance) = exception.plain() else {
        return None;
    };
    instance.dict.borrow().get("targetfile").cloned()
}

fn input_function_error(error: Error, rule: &Rule, wildcards: &Wildcards) -> Error {
    if error.is_unsupported() {
        return error;
    }
    let values: Vec<String> = wildcards
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let message = format!(
        "InputFunctionException in rule {} (wildcards: {}): {}: {}",
        rule.name,
        values.join(", "),
        error.class_name(),
        error.message()
    );
    let mut wrapped = crate::workflow::workflow_error(message);
    wrapped.location = error.location;
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow() -> (tempfile::TempDir, Interpreter, Planner) {
        let folder = tempfile::tempdir().unwrap();
        let interpreter =
            Interpreter::new(Rc::new(python_evaluator::LocalHost::new(folder.path())));
        let globals = interpreter.new_globals("test", None);
        let rules = [
            ("all", vec!["b.x", "b.y", "side"], vec![]),
            ("seed", vec![], vec!["a.{sample}"]),
            ("branch", vec!["a.{sample}"], vec!["b.{sample}"]),
            ("side", vec![], vec!["side"]),
            ("extra", vec![], vec!["extra.txt"]),
        ]
        .into_iter()
        .map(|(name, input, output)| {
            let mut rule = Rule::new(
                name.into(),
                folder.path().into(),
                "Snakefile".into(),
                1,
                globals.clone(),
            );
            rule.input.values = input.into_iter().map(Value::str).collect();
            rule.output.values = output.into_iter().map(Value::str).collect();
            rule
        })
        .collect();
        (folder, interpreter, Planner::new(rules, Vec::new(), false))
    }

    fn names(planner: &Planner, jobs: impl IntoIterator<Item = usize>) -> Vec<String> {
        let mut names: Vec<_> = jobs
            .into_iter()
            .map(|job| {
                let entry = &planner.jobs[job];
                format!(
                    "{}{}",
                    planner.rules[entry.rule].name,
                    entry
                        .wildcards
                        .get("sample")
                        .map(|sample| format!(".{sample}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn limits_until_jobs() {
        for (target, expected) in [
            ("branch", vec!["branch.x", "branch.y", "seed.x", "seed.y"]),
            ("b.x", vec!["branch.x", "seed.x"]),
            ("extra", vec!["extra"]),
            ("extra.txt", vec!["extra"]),
        ] {
            let (_folder, mut interpreter, mut planner) = workflow();
            let roots = planner
                .plan_targets(&mut interpreter, &["all".into()], &[target.into()], &[])
                .unwrap();
            assert_eq!(names(&planner, planner.ordered_jobs(&roots)), expected);
        }
    }

    #[test]
    fn preserves_omitted_ancestors() {
        for (target, expected) in [
            ("branch", vec!["seed.x", "seed.y", "side"]),
            ("a.x", vec!["branch.y", "seed.y", "side"]),
            (
                "not-in-the-dag",
                vec!["all", "branch.x", "branch.y", "seed.x", "seed.y", "side"],
            ),
        ] {
            let (_folder, mut interpreter, mut planner) = workflow();
            let roots = planner
                .plan_targets(&mut interpreter, &["all".into()], &[], &[target.into()])
                .unwrap();
            assert_eq!(names(&planner, planner.ordered_jobs(&roots)), expected);
        }
    }

    #[test]
    fn propagates_forced_jobs() {
        for (target, expected) in [
            (
                "seed",
                vec!["all", "branch.x", "branch.y", "seed.x", "seed.y"],
            ),
            ("b.x", vec!["all", "branch.x"]),
            ("extra", vec!["extra"]),
        ] {
            let (folder, mut interpreter, mut planner) = workflow();
            for output in ["a.x", "a.y", "b.x", "b.y", "side", "extra.txt"] {
                std::fs::write(folder.path().join(output), "current").unwrap();
            }
            planner.forcerun = vec![target.into()];
            let roots = planner
                .plan_targets(&mut interpreter, &["all".into()], &[], &[])
                .unwrap();
            let forced = planner.forced_jobs(&roots);
            assert_eq!(names(&planner, forced.iter().copied()), expected);
            let needrun = planner.needrun(&interpreter, &roots);
            assert!(forced.iter().all(|job| needrun[job]));
            assert!(
                !needrun.iter().any(|(job, needed)| *needed
                    && planner.rules[planner.jobs[*job].rule].name == "side")
            );
        }
    }

    #[test]
    fn limits_forced_targets() {
        let (folder, mut interpreter, mut planner) = workflow();
        for output in ["a.x", "b.x"] {
            std::fs::write(folder.path().join(output), "current").unwrap();
        }
        planner.forceall = true;
        let roots = planner
            .plan_targets(&mut interpreter, &["b.x".into()], &[], &[])
            .unwrap();
        assert_eq!(
            names(&planner, planner.forced_jobs(&roots)),
            ["branch.x", "seed.x"]
        );
        assert!(
            planner
                .needrun(&interpreter, &roots)
                .values()
                .all(|needed| *needed)
        );
    }

    #[test]
    fn limits_forced_jobs() {
        let (_folder, mut interpreter, mut planner) = workflow();
        planner.forceall = true;
        let roots = planner
            .plan_targets(
                &mut interpreter,
                &["all".into()],
                &["branch".into()],
                &["a.x".into()],
            )
            .unwrap();
        assert_eq!(
            names(&planner, planner.forced_jobs(&roots)),
            ["branch.y", "seed.y"]
        );
    }
}
