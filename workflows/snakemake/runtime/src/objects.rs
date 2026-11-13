//! Objects that Snakefiles see: `Namedlist` values like `input` and `wildcards`, and the
//! `rules`, `checkpoints` and `workflow` globals.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use indexmap::IndexMap;
use python_evaluator::interpreter::Operator;
use python_evaluator::{Args, Dict, Error, Interpreter, Native, Value};

use crate::pattern::{self, Wildcards};
use crate::rule::Items;
use crate::workflow::State;

/// Snakemake's `Namedlist`: a list whose items or ranges of items also have names.
pub struct NamedList {
    pub values: Vec<Value>,
    pub names: Vec<(String, usize, Option<usize>)>,
    pub kind: &'static str,
}

impl NamedList {
    pub fn new(kind: &'static str, values: Vec<Value>) -> NamedList {
        NamedList {
            values,
            names: Vec::new(),
            kind,
        }
    }

    pub fn from_items(kind: &'static str, items: &Items) -> NamedList {
        NamedList {
            values: items.values.clone(),
            names: items.names.clone(),
            kind,
        }
    }

    /// `Wildcards`, from wildcard values.
    pub fn wildcards(values: &Wildcards) -> Value {
        let mut list = NamedList::new("Wildcards", Vec::new());
        for (name, value) in values {
            list.values.push(Value::str(value.as_str()));
            list.names.push((name.clone(), list.values.len() - 1, None));
        }
        Value::native(list)
    }

    pub fn value(self) -> Value {
        Value::native(self)
    }

    /// The item or items of a name.
    pub fn named(&self, name: &str) -> Option<Value> {
        let (_, start, end) = self
            .names
            .iter()
            .find(|(candidate, _, _)| candidate == name)?;
        Some(match end {
            None => self.values.get(*start).cloned().unwrap_or(Value::None),
            Some(end) => Value::native(NamedList::new(
                "Namedlist",
                self.values[(*start).min(self.values.len())..(*end).min(self.values.len())]
                    .to_vec(),
            )),
        })
    }

    pub fn keys(&self) -> Vec<String> {
        self.names.iter().map(|(name, _, _)| name.clone()).collect()
    }
}

/// Total size of files in bytes, as far as the host knows them.
fn total_size(interpreter: &mut Interpreter, values: &[Value]) -> u64 {
    let mut total = 0;
    for value in values {
        if let Some(path) = value.as_str() {
            let resolved = interpreter.host.resolve(path);
            total += interpreter.host.stat(&resolved).map_or(0, |stat| stat.size);
        }
    }
    total
}

impl Native for NamedList {
    fn type_name(&self) -> &str {
        self.kind
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        matches!(class, "list" | "Namedlist") || class == self.kind
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(self.values.clone()))
    }

    fn length(&self) -> Option<usize> {
        Some(self.values.len())
    }

    fn truthy(&self) -> Option<bool> {
        Some(!self.values.is_empty())
    }

    fn contains(&self, interpreter: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        let list = Value::list(self.values.clone());
        interpreter.contains(&list, item).map(Some)
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        if let Some(name) = key.as_str() {
            return match self.named(name) {
                Some(value) => Ok(Some(value)),
                None => Err(Error::attribute_error(format!(
                    "'{}' object has no attribute '{name}'",
                    self.kind
                ))),
            };
        }
        interpreter
            .get_item(&Value::list(self.values.clone()), key)
            .map(Some)
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Error> {
        let mut parts = Vec::with_capacity(self.values.len());
        for value in &self.values {
            parts.push(interpreter.to_str(value)?);
        }
        Ok(Some(parts.join(" ")))
    }

    fn repr(&self, interpreter: &mut Interpreter) -> Result<String, Error> {
        interpreter.repr(&Value::list(self.values.clone()))
    }

    fn key_equal(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<NamedList>()
            .is_some_and(|other| {
                other.values.len() == self.values.len()
                    && other
                        .values
                        .iter()
                        .zip(&self.values)
                        .all(|(a, b)| python_evaluator::value::key_equal(a, b))
            })
    }

    fn key_hash(&self, state: &mut dyn std::hash::Hasher) {
        state.write_usize(self.values.len());
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let mine = Value::list(self.values.clone());
        let theirs = match other.as_native::<NamedList>() {
            Some(other) => Value::list(other.values.clone()),
            None => other.clone(),
        };
        if !matches!(theirs.plain(), Value::List(_) | Value::Tuple(_))
            && operator != Operator::Eq
            && operator != Operator::NotEq
        {
            return Ok(None);
        }
        let (a, b) = if reflected {
            (theirs, mine)
        } else {
            (mine, theirs)
        };
        interpreter.binary(operator, &a, &b).map(Some)
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        if let Some(value) = self.named(name) {
            return Ok(Some(value));
        }
        let this = this.clone();
        let list = move || {
            this.as_native::<NamedList>()
                .map(|list| (list.values.clone(), list.keys(), list.names.clone()))
        };
        let (values, keys, _) = list().unwrap_or_default();
        Ok(Some(match name {
            "get" => {
                let this_list = self.values.clone();
                let names = self.names.clone();
                let kind = self.kind;
                Value::builtin("get", move |_, args| {
                    args.expect("get", 1, 2)?;
                    let key = args.positional[0].as_str().unwrap_or_default().to_owned();
                    let list = NamedList {
                        values: this_list.clone(),
                        names: names.clone(),
                        kind,
                    };
                    Ok(list
                        .named(&key)
                        .unwrap_or_else(|| args.positional.get(1).cloned().unwrap_or(Value::None)))
                })
            }
            "keys" => Value::builtin("keys", move |_, _| {
                Ok(Value::list(
                    keys.iter().map(|key| Value::str(key.as_str())).collect(),
                ))
            }),
            "items" => {
                let pairs: Vec<Value> = self
                    .keys()
                    .into_iter()
                    .filter_map(|key| {
                        self.named(&key)
                            .map(|value| Value::tuple(vec![Value::str(key), value]))
                    })
                    .collect();
                Value::builtin("items", move |_, _| Ok(Value::list(pairs.clone())))
            }
            "values" => Value::builtin("values", move |_, _| Ok(Value::list(values.clone()))),
            "size_files" | "size_files_kb" | "size_files_mb" | "size_files_gb" => {
                let divisor = match name {
                    "size_files_kb" => 1024.0,
                    "size_files_mb" => 1024.0 * 1024.0,
                    "size_files_gb" => 1024.0 * 1024.0 * 1024.0,
                    _ => 1.0,
                };
                let mut sizes = Vec::new();
                for value in &self.values {
                    let size =
                        total_size(interpreter, std::slice::from_ref(value)) as f64 / divisor;
                    sizes.push(if divisor == 1.0 {
                        Value::Int(size as i64)
                    } else {
                        Value::Float(size)
                    });
                }
                Value::list(sizes)
            }
            "size" => Value::Int(total_size(interpreter, &self.values) as i64),
            "size_kb" => Value::Float(total_size(interpreter, &self.values) as f64 / 1024.0),
            "size_mb" => {
                Value::Float(total_size(interpreter, &self.values) as f64 / 1024.0 / 1024.0)
            }
            "size_gb" => Value::Float(
                total_size(interpreter, &self.values) as f64 / 1024.0 / 1024.0 / 1024.0,
            ),
            "size_tempfiles" => Value::Int(0),
            "index" | "sort" => {
                return Err(Error::attribute_error(format!(
                    "{name}() cannot be used on snakemake input, output, resources etc.; instead it is a valid name for items on those objects. If you want e.g. to sort, convert to a plain list before or directly use sorted() on the object."
                )));
            }
            _ => {
                return Err(Error::attribute_error(format!(
                    "'{}' object has no attribute '{name}'",
                    self.kind
                )));
            }
        }))
    }
}

/// Rule names as the code of a module knows them, and as the workflow registers them.
pub type RuleNames = Rc<RefCell<IndexMap<String, String>>>;

/// The workflow name of a rule that code names; a module only knows its own rules.
fn resolve(state: &State, names: Option<&RuleNames>, name: &str) -> Option<String> {
    match names {
        Some(names) => names.borrow().get(name).cloned(),
        None => state.rule(name).map(|rule| rule.name.clone()),
    }
}

/// The `rules` global: `rules.<name>` for each rule defined so far.
pub struct RulesObject {
    state: Rc<RefCell<State>>,
    names: Option<RuleNames>,
}

impl RulesObject {
    pub fn new(state: Rc<RefCell<State>>) -> RulesObject {
        RulesObject { state, names: None }
    }

    /// The `rules` of a module, which knows its rules by their names in the module.
    pub fn scoped(state: Rc<RefCell<State>>, names: RuleNames) -> RulesObject {
        RulesObject {
            state,
            names: Some(names),
        }
    }
}

impl Native for RulesObject {
    fn type_name(&self) -> &str {
        "Rules"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let found = resolve(&self.state.borrow(), self.names.as_ref(), name);
        let Some(global) = found else {
            return Err(Error::attribute_error(format!(
                "'Rules' object has no attribute '{name}'"
            )));
        };
        Ok(Some(Value::native(RuleProxy {
            state: self.state.clone(),
            name: global,
        })))
    }
}

/// `rules.<name>`: the patterns and values of a rule.
pub struct RuleProxy {
    pub state: Rc<RefCell<State>>,
    pub name: String,
}

impl Native for RuleProxy {
    fn type_name(&self) -> &str {
        "RuleProxy"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let state = self.state.borrow();
        let Some(rule) = state.rule(&self.name) else {
            return Ok(None);
        };
        // Files of the rule as Snakemake's `RuleProxy` gives them: without the constraints of
        // their wildcards and without flags, except the path modification of a module.
        let file = |value: &Value| -> Value {
            let Some(text) = value.as_str() else {
                return value.clone();
            };
            let mut file = Value::str(pattern::strip_constraints(text));
            if let Some(modified) = value.tag("path_modified") {
                file = file.with_tag("path_modified", modified.clone());
            }
            file.with_tag("rule", Value::str(rule.name.as_str()))
        };
        let files = |items: &crate::rule::Items, kind: &'static str| {
            let mut items = items.clone();
            items.values = items.values.iter().map(file).collect();
            NamedList::from_items(kind, &items).value()
        };
        Ok(Some(match name {
            "output" => files(&rule.output, "OutputFiles"),
            "input" => NamedList::from_items("InputFiles", &rule.input).value(),
            "log" => files(&rule.log, "Log"),
            "params" => NamedList::from_items("Params", &rule.params).value(),
            "benchmark" => rule.benchmark.as_ref().map_or(Value::None, file),
            "name" => Value::str(rule.name.as_str()),
            "rule" => this.clone(),
            "basedir" => Value::str(rule.basedir.to_string_lossy().as_ref()),
            "snakefile" => Value::str(rule.snakefile.as_str()),
            "wildcard_names" => Value::set(
                rule.wildcard_names()
                    .into_iter()
                    .map(|name| python_evaluator::Key(Value::str(name)))
                    .collect(),
            ),
            "message" => rule.message.clone().unwrap_or(Value::None),
            "is_checkpoint" => Value::Bool(rule.checkpoint),
            _ => return Ok(None),
        }))
    }
}

/// The `checkpoints` global.
pub struct CheckpointsObject {
    state: Rc<RefCell<State>>,
    names: Option<RuleNames>,
}

impl CheckpointsObject {
    pub fn new(state: Rc<RefCell<State>>) -> CheckpointsObject {
        CheckpointsObject { state, names: None }
    }

    /// The `checkpoints` of a module.
    pub fn scoped(state: Rc<RefCell<State>>, names: RuleNames) -> CheckpointsObject {
        CheckpointsObject {
            state,
            names: Some(names),
        }
    }
}

impl Native for CheckpointsObject {
    fn type_name(&self) -> &str {
        "Checkpoints"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let found = {
            let state = self.state.borrow();
            resolve(&state, self.names.as_ref(), name)
                .filter(|global| state.rule(global).is_some_and(|rule| rule.checkpoint))
        };
        let Some(global) = found else {
            return Err(Error::attribute_error(format!(
                "'Checkpoints' object has no attribute '{name}'"
            )));
        };
        Ok(Some(Value::native(Checkpoint {
            state: self.state.clone(),
            name: global,
        })))
    }
}

thread_local! {
    /// The first output of each checkpoint that `get` consulted, since the last `take`.
    static CONSULTED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    static CHECKPOINTS_READY: RefCell<Option<Rc<RefCell<HashSet<String>>>>> = const { RefCell::new(None) };
}

pub fn set_checkpoint_ready(ready: Option<&Rc<RefCell<HashSet<String>>>>) {
    CHECKPOINTS_READY.with(|allowed| *allowed.borrow_mut() = ready.cloned());
}

/// The outputs and rules of the checkpoints that input functions asked for since the last call;
/// the jobs of those functions run after the checkpoints.
pub fn take_consulted() -> Vec<(String, String)> {
    CONSULTED.with(|consulted| std::mem::take(&mut *consulted.borrow_mut()))
}

/// The exception class that `checkpoints.<name>.get` raises before the checkpoint ran.
pub fn incomplete_checkpoint_class() -> Rc<python_evaluator::value::Class> {
    python_evaluator::types::library_exception(
        "IncompleteCheckpointException",
        "snakemake.exceptions",
        "Exception",
    )
}

/// `checkpoints.<name>`.
pub struct Checkpoint {
    state: Rc<RefCell<State>>,
    name: String,
}

impl Native for Checkpoint {
    fn type_name(&self) -> &str {
        "Checkpoint"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let state = self.state.clone();
        let rule_name = self.name.clone();
        Ok(match name {
            "get" => Some(Value::builtin("get", move |interpreter, args| {
                if !args.positional.is_empty() {
                    return Err(Error::type_error("get() takes keyword arguments only"));
                }
                checkpoint_get(interpreter, &state, &rule_name, args)
            })),
            "rule" => Some(Value::native(RuleProxy {
                state: self.state.clone(),
                name: self.name.clone(),
            })),
            _ => None,
        })
    }
}

fn checkpoint_get(
    interpreter: &mut Interpreter,
    state: &Rc<RefCell<State>>,
    rule_name: &str,
    args: Args,
) -> Result<Value, Error> {
    let mut wildcards = Wildcards::new();
    for (name, value) in &args.keywords {
        wildcards.insert(name.to_string(), interpreter.to_str(value)?);
    }
    let (outputs, names) = {
        let state = state.borrow();
        let rule = state
            .rule(rule_name)
            .ok_or_else(|| Error::runtime_error("unknown checkpoint"))?;
        let missing: Vec<String> = rule
            .wildcard_names()
            .into_iter()
            .filter(|name| !wildcards.contains_key(name))
            .collect();
        if !missing.is_empty() {
            return Err(crate::workflow::workflow_error(format!(
                "Missing wildcard values for {}",
                missing.join(", ")
            )));
        }
        let mut outputs = Vec::new();
        for value in &rule.output.values {
            let text = value.as_str().unwrap_or_default();
            let filled = pattern::apply(text, &wildcards).map_err(|name| {
                crate::workflow::workflow_error(format!("Missing wildcard value for {name}"))
            })?;
            outputs.push(
                crate::rule::retag(value, Value::str(filled))
                    .with_tag("rule", Value::str(rule_name)),
            );
        }
        (outputs, rule.output.names.clone())
    };
    let created = outputs.iter().all(|output| {
        let path = interpreter
            .host
            .resolve(output.as_str().unwrap_or_default());
        interpreter.host.stat(&path).is_some()
    });
    let ready = if let Some(first) = outputs.first().and_then(Value::as_str) {
        CONSULTED.with(|consulted| {
            let mut consulted = consulted.borrow_mut();
            if !consulted
                .iter()
                .any(|(file, rule)| file == first && rule == rule_name)
            {
                consulted.push((first.to_owned(), rule_name.to_owned()));
            }
        });
        CHECKPOINTS_READY.with(|allowed| {
            allowed
                .borrow()
                .as_ref()
                .is_none_or(|ready| ready.borrow().contains(first))
        })
    } else {
        true
    };
    if created && ready {
        let output = NamedList {
            values: outputs,
            names,
            kind: "OutputFiles",
        };
        return Ok(Value::native(CheckpointJob {
            output: output.value(),
            rule: rule_name.to_owned(),
        }));
    }
    let target = outputs.first().cloned().unwrap_or(Value::None);
    let class = incomplete_checkpoint_class();
    let exception =
        python_evaluator::error::exception(&class, vec![Value::str(rule_name), target.clone()]);
    if let Value::Instance(instance) = &exception {
        instance
            .dict
            .borrow_mut()
            .insert("targetfile".into(), target);
        instance
            .dict
            .borrow_mut()
            .insert("rule".into(), Value::str(rule_name));
    }
    Err(Error::from_value(exception))
}

/// What `checkpoints.<name>.get` gives once the checkpoint ran.
pub struct CheckpointJob {
    output: Value,
    rule: String,
}

impl Native for CheckpointJob {
    fn type_name(&self) -> &str {
        "CheckpointJob"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "output" => Some(self.output.clone()),
            "rule" => Some(Value::str(self.rule.as_str())),
            _ => None,
        })
    }
}

/// The `workflow` global.
pub struct WorkflowObject(pub Rc<RefCell<State>>);

impl Native for WorkflowObject {
    fn type_name(&self) -> &str {
        "Workflow"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let state = self.0.clone();
        let borrowed = self.0.borrow();
        let text = |path: &std::path::Path| Value::str(path.to_string_lossy().as_ref());
        Ok(Some(match name {
            "basedir" => text(
                borrowed
                    .main_snakefile
                    .parent()
                    .unwrap_or(std::path::Path::new(".")),
            ),
            "current_basedir" => {
                text(&borrowed.current_basedir()).with_tag("source_file", Value::Bool(true))
            }
            "snakefile" => text(
                borrowed
                    .include_stack
                    .last()
                    .unwrap_or(&borrowed.main_snakefile),
            ),
            "main_snakefile" => text(&borrowed.main_snakefile),
            "pathvars" => Value::native(crate::pathvars::PathvarsObject(
                borrowed.modifier().pathvars.clone(),
            )),
            "workdir_init" => text(&borrowed.workdir),
            "config" => borrowed.config.clone(),
            "globals" => Value::native(python_evaluator::builtins::GlobalsDict(
                borrowed.globals.clone(),
            )),
            "cores" | "_cores" | "nodes" => Value::Int(borrowed.cores),
            "use_conda" => Value::Bool(true),
            "use_singularity" | "use_apptainer" | "use_envmodules" => Value::Bool(false),
            "run_local" | "is_main_process" | "local_exec" | "non_local_exec" => Value::Bool(
                matches!(name, "run_local" | "is_main_process" | "local_exec"),
            ),
            "remote_exec" | "dryrun" | "touch" | "immediate_submit" => Value::Bool(false),
            // The settings of storage plugins by their name; a run of uwe has none.
            "storage_provider_settings" => Value::dict(python_evaluator::Dict::default()),
            "default_remote_prefix"
            | "default_remote_provider"
            | "conda_prefix"
            | "singularity_prefix" => Value::None,
            "configfiles" | "overwrite_configfiles" => {
                let files = if name == "configfiles" {
                    &borrowed.configfiles
                } else {
                    &borrowed.overwrite_configfiles
                };
                Value::list(files.iter().map(|file| Value::str(file.as_str())).collect())
            }
            "config_settings" => {
                let mut record = python_evaluator::Dict::default();
                record.insert(
                    "configfiles",
                    Value::list(
                        borrowed
                            .overwrite_configfiles
                            .iter()
                            .map(|file| Value::str(file.as_str()))
                            .collect(),
                    ),
                );
                record.insert("config", borrowed.overwrite_config.clone());
                record.insert("overwrite_config", borrowed.overwrite_config.clone());
                Value::native(python_evaluator::builtins::Record(
                    RefCell::new(record),
                    "ConfigSettings",
                ))
            }
            "storage_settings"
            | "deployment_settings"
            | "output_settings"
            | "execution_settings"
            | "resource_settings"
            | "workflow_settings"
            | "dag_settings"
            | "remote_execution_settings" => {
                let mut record = python_evaluator::Dict::default();
                record.insert("default_storage_provider", Value::None);
                record.insert("default_storage_prefix", Value::str(""));
                record.insert("deployment_method", Value::set(Default::default()));
                record.insert("cores", Value::Int(borrowed.cores));
                record.insert("nodes", Value::Int(1));
                record.insert("keep_going", Value::Bool(false));
                let prefix = borrowed.profile.wrapper_prefix.as_deref();
                let prefix = prefix.unwrap_or(crate::embedded::wrapper::DEFAULT_PREFIX);
                record.insert("wrapper_prefix", Value::str(prefix));
                Value::native(python_evaluator::builtins::Record(
                    RefCell::new(record),
                    "Settings",
                ))
            }
            "source_path" => {
                drop(borrowed);
                Value::builtin("source_path", move |interpreter, args| {
                    args.expect("source_path", 1, 1)?;
                    let relative =
                        python_evaluator::modules::path_text(interpreter, &args.positional[0])?;
                    let base = state.borrow().current_basedir();
                    Ok(Value::str(base.join(relative).to_string_lossy().as_ref()))
                })
            }
            "get_rule" => {
                drop(borrowed);
                Value::builtin("get_rule", move |_, args| {
                    args.expect("get_rule", 1, 1)?;
                    let name = args.positional[0].as_str().unwrap_or_default().to_owned();
                    if state.borrow().rule(&name).is_none() {
                        return Err(crate::workflow::workflow_error(format!(
                            "Rule {name} is not defined in this workflow."
                        )));
                    }
                    Ok(Value::native(RuleProxy {
                        state: state.clone(),
                        name,
                    }))
                })
            }
            "rules" => {
                let names: Vec<String> = borrowed
                    .rules
                    .iter()
                    .map(|rule| rule.name.clone())
                    .collect();
                drop(borrowed);
                Value::list(
                    names
                        .into_iter()
                        .map(|name| {
                            Value::native(RuleProxy {
                                state: state.clone(),
                                name,
                            })
                        })
                        .collect(),
                )
            }
            "modules" => Value::dict(Default::default()),
            "included" => Value::list(borrowed.included.iter().map(|path| text(path)).collect()),
            "wildcard_constraints" => Value::dict_of(
                borrowed
                    .global_constraints
                    .iter()
                    .map(|(name, value)| (name.clone(), Value::str(value.as_str()))),
            ),
            "global_resources" => {
                let mut resources = Dict::default();
                resources.insert("_cores", Value::Int(1));
                resources.insert("_nodes", Value::Int(i64::MAX));
                for (name, value) in &borrowed.global_resources {
                    resources.insert(name.as_str(), value.clone());
                }
                Value::dict(resources)
            }
            "register_resource" => Value::builtin("register_resource", move |interpreter, args| {
                args.expect("register_resource", 2, 2)?;
                let name = interpreter.to_str(&args.positional[0])?;
                let value = args.positional[1].clone();
                state.borrow_mut().global_resources.insert(name, value);
                Ok(Value::None)
            }),
            // A name that Snakemake's workflow does not have is an error of the workflow.
            _ if !WORKFLOW_ATTRIBUTES.contains(&name) => {
                return Err(Error::attribute_error(format!(
                    "'Workflow' object has no attribute '{name}'"
                )));
            }
            _ => {
                let _ = interpreter;
                return Ok(None);
            }
        }))
    }
}

/// The attributes of Snakemake 9.27's `Workflow`.
const WORKFLOW_ATTRIBUTES: &[&str] = &[
    "add_rule",
    "archive",
    "async_run",
    "attempt",
    "basedir",
    "benchmark",
    "cache_rule",
    "check",
    "check_cache_rules",
    "check_envvars",
    "check_localrules",
    "checkpoints",
    "cleanup_metadata",
    "cleanup_shadow",
    "cleanup_source_archive",
    "concrete_files",
    "conda",
    "conda_base_path",
    "conda_cleanup_envs",
    "conda_create_envs",
    "conda_list_envs",
    "config",
    "config_settings",
    "configfile",
    "configfiles",
    "container",
    "container_cleanup_images",
    "containerize",
    "containerized",
    "cores",
    "create_report",
    "current_basedir",
    "cwl",
    "dag",
    "dag_settings",
    "default_target",
    "default_target_rule",
    "delete_output",
    "deployment_settings",
    "docstring",
    "dryrun",
    "enable_cache",
    "envmodules",
    "envvars",
    "exec_mode",
    "execute",
    "execution_settings",
    "executor_plugin",
    "executor_settings",
    "export_cwl",
    "generate_unit_tests",
    "get_rule",
    "get_rule_source",
    "global_conda",
    "global_container",
    "global_containerized",
    "global_report_settings",
    "global_resources",
    "global_wildcard_constraints",
    "globals",
    "group",
    "group_settings",
    "handover",
    "include",
    "included",
    "included_stack",
    "info_header",
    "injected_conda_envs",
    "input",
    "inputfile",
    "iocache",
    "is_local",
    "is_main_process",
    "is_rule",
    "keep_storage_local_at_runtime",
    "linemaps",
    "lint",
    "list_changes",
    "list_resources",
    "list_rules",
    "list_untracked",
    "local_exec",
    "localrule",
    "localrules",
    "log",
    "log_metadata_info",
    "log_missing_metadata_info",
    "log_outdated_metadata_info",
    "log_provenance_info",
    "log_rulegraph",
    "log_workflow_runtime",
    "logger_manager",
    "main_snakefile",
    "message",
    "modifier",
    "modifier_stack",
    "module",
    "modules",
    "name",
    "nodes",
    "non_local_exec",
    "non_local_exec_or_dryrun",
    "norun",
    "notebook",
    "onerror",
    "onstart",
    "onsuccess",
    "output",
    "output_file_cache",
    "output_settings",
    "overwrite_configfiles",
    "overwrite_workdir",
    "params",
    "parent_groupids",
    "pathvars",
    "pepfile",
    "pepschema",
    "persistence",
    "printd3dag",
    "printdag",
    "printfilegraph",
    "printrulegraph",
    "priority",
    "register_envvars",
    "register_pathvars",
    "register_resource",
    "register_wildcard_constraints",
    "remote_exec",
    "remote_execution_settings",
    "report",
    "report_text",
    "rerun_triggers",
    "resource_scopes",
    "resource_settings",
    "resources",
    "resourcescope",
    "retries",
    "rule",
    "rule_count",
    "rule_pathvars",
    "ruleorder",
    "rules",
    "run",
    "rundir",
    "runtime_paths",
    "scattergather",
    "scheduler",
    "scheduling_settings",
    "script",
    "set_default_input_flags",
    "set_default_output_flags",
    "set_pepfile",
    "shadow",
    "shellcmd",
    "snakefile",
    "snakemake_tmp_dir",
    "source_archive",
    "source_cache_path",
    "source_path",
    "sourcecache",
    "spawned_job_args_factory",
    "start_time",
    "storage_provider_settings",
    "storage_registry",
    "storage_settings",
    "subprocess_exec",
    "summary",
    "tear_down",
    "template_engine",
    "threads",
    "touch",
    "unlock",
    "upload_sources",
    "use_threads",
    "userule",
    "vanilla_globals",
    "wildcard_constraints",
    "workdir",
    "workdir_init",
    "workflow_settings",
    "wrapper",
    "write_source_archive",
];
