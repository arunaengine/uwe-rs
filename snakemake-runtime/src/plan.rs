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

    fn plan_job_inner(
        &mut self,
        interpreter: &mut Interpreter,
        rule_index: usize,
        wildcards: &Wildcards,
        target: Option<&str>,
    ) -> Result<Job, Failure> {
        let rule = self.rules[rule_index].clone();
        crate::objects::take_consulted();
        let (input, incomplete) =
            expand_input(interpreter, &rule, wildcards).map_err(Failure::Error)?;
        let checkpoints = crate::objects::take_consulted();
        let fill = |items: &Items, what: &str| -> Result<Items, Failure> {
            let mut filled = items.clone();
            filled.values = items
                .values
                .iter()
                .map(|value| match value.as_str() {
                    Some(text) => pattern::apply(text, wildcards).map(|text| retag(value, Value::str(text))).map_err(|name| {
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
            dependencies: Vec::new(),
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
        let inputs: Vec<(String, Option<String>, bool)> = {
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
                inputs.push((text.to_owned(), dependency, stored));
            }
            inputs
        };
        for (file, dependency, stored) in inputs {
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
                // A file in storage counts as present.
                Ok(None) if stored => {}
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
                    if stored || self.exists(interpreter, &file) {
                        continue;
                    }
                    return Err(failure);
                }
            }
        }
        Ok(job)
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
            self.visit_needrun(interpreter, *root, &mut result, &mut HashSet::new());
        }
        result
    }

    fn visit_needrun(
        &self,
        interpreter: &Interpreter,
        job: usize,
        result: &mut HashMap<usize, bool>,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        if let Some(found) = result.get(&job) {
            return *found;
        }
        if !visiting.insert(job) {
            return false;
        }
        let mut needed = false;
        for (_, dependency) in &self.jobs[job].dependencies {
            needed |= self.visit_needrun(interpreter, *dependency, result, visiting);
        }
        let outputs = self.jobs[job].output_files();
        let own_outputs: Vec<String> = self.jobs[job]
            .output
            .values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        needed |= own_outputs.is_empty()
            || self.jobs[job].incomplete
            || outputs
                .iter()
                .filter(|file| own_outputs.contains(file))
                .any(|file| !self.exists(interpreter, file));
        result.insert(job, needed);
        needed
    }
}

/// The group id that input functions with a `groupid` parameter get. Snakemake gives a job of
/// a group the id of its group job, and other jobs `local` in a local run. iunctus runs each
/// job as a task of its own, so a job of a group is a group of one, with an id that its rule
/// and wildcards determine, in the form of the UUIDs of Snakemake.
fn group_id(rule: &Rule, wildcards: &Wildcards) -> String {
    if rule.group.is_none() {
        return "local".into();
    }
    let mut text = rule.name.clone();
    for (name, value) in wildcards {
        text.push_str(&format!(",{name}={value}"));
    }
    // Two FNV-1a hashes with different offsets give 128 bits.
    let hash = |offset: u64| {
        text.bytes().fold(offset, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let hex = format!(
        "{:016x}{:016x}",
        hash(0xcbf2_9ce4_8422_2325),
        hash(0x6c62_272e_07bb_0142)
    );
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

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
                let aux = [("groupid", Value::str(group_id(rule, wildcards)))];
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
        let text = pathvars.apply(&text)?;
        let filled = pattern::apply(&text, wildcards).unwrap_or(text);
        let mut value = retag(&file, Value::str(filled));
        if let Some(tags) = function.tags() {
            for (flag, flag_value) in tags {
                if &**flag != "unpack" && value.tag(flag).is_none() {
                    value = value.with_tag(flag, flag_value.clone());
                }
            }
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
