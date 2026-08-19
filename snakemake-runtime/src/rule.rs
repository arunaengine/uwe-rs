//! Rules as the Snakefile defines them: file patterns, input functions and the other
//! directive values, kept as Python values until jobs evaluate them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use indexmap::IndexMap;
use python_evaluator::{Error, Scope, Value};

use crate::pattern;

/// A list of directive values with names for some items or ranges of items, like
/// Snakemake's `Namedlist`.
#[derive(Clone, Default)]
pub struct Items {
    pub values: Vec<Value>,
    /// A name, the first item, and the end of the range for names of several items.
    pub names: Vec<(String, usize, Option<usize>)>,
}

/// The names that Snakemake's named lists keep for themselves, their methods and attributes,
/// except `index` and `sort`, which a name may take (`Namedlist._set_name` of Snakemake
/// 9.27.0); input files have their sizes besides.
const RESERVED: [&str; 22] = [
    "_add_name",
    "_allitems",
    "_clone",
    "_get_names",
    "_insert_items",
    "_plainstrings",
    "_set_name",
    "_stripped_constraints",
    "_take_names",
    "append",
    "clear",
    "copy",
    "count",
    "extend",
    "get",
    "insert",
    "items",
    "keys",
    "pop",
    "remove",
    "reverse",
    "update",
];
const RESERVED_FOR_INPUTS: [&str; 11] = [
    "_predicated_size_files",
    "size",
    "size_files",
    "size_files_gb",
    "size_files_kb",
    "size_files_mb",
    "size_gb",
    "size_kb",
    "size_mb",
    "size_tempfiles",
    "temp_size_mb",
];

/// Fails, as Snakemake does, for a name of an input, output, log or param that the named list
/// keeps for itself, like `items`.
pub fn check_name(name: &str, input: bool) -> Result<(), Error> {
    if RESERVED.contains(&name) || (input && RESERVED_FOR_INPUTS.contains(&name)) {
        return Err(Error::attribute_error(format!(
            "invalid name for input, output, wildcard, params or log: {name} is reserved for \
             internal use"
        )));
    }
    Ok(())
}

impl Items {
    pub fn push(&mut self, value: Value) {
        self.values.push(value);
    }

    pub fn add_name(&mut self, name: &str) {
        let index = self.values.len().saturating_sub(1);
        self.set_name(name, index, None);
    }

    pub fn set_name(&mut self, name: &str, start: usize, end: Option<usize>) {
        self.names.retain(|(existing, _, _)| existing != name);
        self.names.push((name.to_owned(), start, end));
    }

    /// The items with their names, one entry per name or unnamed item, in order.
    pub fn named_entries(&self) -> Vec<(Option<String>, Vec<Value>, bool)> {
        let mut names = self.names.clone();
        names.sort_by_key(|(_, start, end)| (*start, end.unwrap_or(*start + 1)));
        let mut entries = Vec::new();
        let mut next = 0;
        for (name, start, end) in names {
            if start > next {
                for value in &self.values[next..start] {
                    entries.push((None, vec![value.clone()], false));
                }
            }
            let (stop, many) = match end {
                Some(end) => (end, true),
                None => (start + 1, false),
            };
            let stop = stop.min(self.values.len());
            if start < stop || many {
                entries.push((
                    Some(name),
                    self.values[start.min(stop)..stop].to_vec(),
                    many,
                ));
            }
            next = stop.max(next);
        }
        for value in self.values.iter().skip(next) {
            entries.push((None, vec![value.clone()], false));
        }
        entries
    }
}

/// How the jobs of a rule run.
#[derive(Clone)]
pub enum Action {
    /// No directive: a rule that only collects its inputs, like `rule all`.
    None,
    Shell(Value),
    Script(Value),
    Notebook(Value),
    Wrapper(Value),
    TemplateEngine(Value),
    Cwl(Value),
    /// A `run:` block, by its source text with the indentation it has in the Snakefile; it
    /// runs at job time.
    Run(String),
}

#[derive(Clone)]
pub struct Rule {
    pub name: String,
    pub checkpoint: bool,
    pub input: Items,
    pub output: Items,
    pub log: Items,
    pub benchmark: Option<Value>,
    pub params: Items,
    pub threads: Option<Value>,
    pub resources: IndexMap<String, Value>,
    pub message: Option<Value>,
    pub conda: Option<Value>,
    /// The rule's container, `Some(None)` when it asks for none.
    pub container: Option<Option<Value>>,
    pub envmodules: Vec<Value>,
    pub wildcard_constraints: IndexMap<String, String>,
    pub priority: Option<Value>,
    pub retries: Option<Value>,
    pub group: Option<Value>,
    pub localrule: bool,
    pub action: Action,
    /// The f-string of a `shell:` directive, which Snakemake evaluates when the job runs,
    /// with the names of the job, like `output`.
    pub shell_code: Option<Rc<ShellCode>>,
    /// The folder of the Snakefile that defines the rule; scripts and conda files are
    /// relative to it.
    pub basedir: PathBuf,
    pub snakefile: String,
    pub line: usize,
    /// The globals of the Snakefile or module, which shell commands may name.
    pub globals: Rc<Scope>,
    /// Rules that produce named input files, from `rules.<name>.output` references.
    pub docstring: Option<String>,
    /// The path variables that the rule's files may contain.
    pub pathvars: Rc<crate::pathvars::Pathvars>,
}

/// Code of a Snakefile with the source it comes from.
pub struct ShellCode {
    pub expression: python_evaluator::ruff_python_ast::Expr,
    pub source: Rc<python_evaluator::Source>,
}

impl Rule {
    pub fn new(
        name: String,
        basedir: PathBuf,
        snakefile: String,
        line: usize,
        globals: Rc<Scope>,
    ) -> Rule {
        Rule {
            name,
            checkpoint: false,
            input: Items::default(),
            output: Items::default(),
            log: Items::default(),
            benchmark: None,
            params: Items::default(),
            threads: None,
            resources: IndexMap::new(),
            message: None,
            conda: None,
            container: None,
            envmodules: Vec::new(),
            wildcard_constraints: IndexMap::new(),
            priority: None,
            retries: None,
            group: None,
            localrule: false,
            action: Action::None,
            shell_code: None,
            basedir,
            snakefile,
            line,
            globals,
            docstring: None,
            pathvars: Rc::new(crate::pathvars::Pathvars::defaults()),
        }
    }

    /// The output, log and benchmark patterns, which jobs of the rule produce.
    pub fn products(&self) -> Vec<String> {
        let mut products: Vec<String> = Vec::new();
        for value in self.output.values.iter().chain(self.log.values.iter()) {
            if let Some(text) = value.as_str() {
                products.push(text.to_owned());
            }
        }
        if let Some(benchmark) = &self.benchmark
            && let Some(text) = benchmark.as_str()
        {
            products.push(text.to_owned());
        }
        products
    }

    /// The wildcard names of the output patterns.
    pub fn wildcard_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for product in self.products() {
            for name in pattern::wildcard_names(&product) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    pub fn has_wildcards(&self) -> bool {
        !self.wildcard_names().is_empty()
    }

    /// Adds constraints of the rule and the workflow to the output, log and benchmark
    /// patterns.
    pub fn update_constraints(&mut self, global: &IndexMap<String, String>) {
        let constraints = self.wildcard_constraints.clone();
        let update = |value: &Value| -> Value {
            match value.as_str() {
                Some(text) => {
                    let updated = pattern::update_constraints(text, &constraints, global);
                    retag(value, Value::str(updated))
                }
                None => value.clone(),
            }
        };
        self.output.values = self.output.values.iter().map(update).collect();
        self.log.values = self.log.values.iter().map(update).collect();
        if let Some(benchmark) = &self.benchmark {
            self.benchmark = Some(update(benchmark));
        }
    }
}

/// A new value with the flags of an old one.
pub fn retag(old: &Value, new: Value) -> Value {
    let mut result = new;
    if let Some(tags) = old.tags() {
        for (name, flag) in tags {
            result = result.with_tag(name, flag.clone());
        }
    }
    result
}

/// Snakemake's `check_output_duplicates`: a rule may not name the same output twice. Entries
/// are named by their name, or by their number among the unnamed ones.
pub fn check_output_duplicates(rule: &str, output: &Items) -> Result<(), Error> {
    let mut seen: Vec<(&str, String)> = Vec::new();
    let mut unnamed = 0;
    for (index, value) in output.values.iter().enumerate() {
        let name = output.names.iter().find(|(_, start, end)| match end {
            Some(end) => (*start..*end).contains(&index),
            None => index == *start,
        });
        let label = match name {
            Some((name, _, _)) => name.clone(),
            None => {
                unnamed += 1;
                (unnamed - 1).to_string()
            }
        };
        let Some(text) = value.as_str().filter(|text| !text.is_empty()) else {
            continue;
        };
        if let Some((_, earlier)) = seen.iter().find(|(seen, _)| *seen == text) {
            return Err(crate::workflow::workflow_error(format!(
                "Duplicate output file pattern in rule {rule}. First two duplicate for entries \
                 {earlier} and {label}."
            )));
        }
        seen.push((text, label));
    }
    Ok(())
}
