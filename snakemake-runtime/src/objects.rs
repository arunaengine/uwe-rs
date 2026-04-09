//! Objects that Snakefiles see: `Namedlist` values like `input` and `wildcards`, and the
//! `rules`, `checkpoints` and `workflow` globals.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use python_evaluator::interpreter::Operator;
use python_evaluator::{Args, Error, Interpreter, Native, Value};

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
            _ => return Ok(None),
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
        let tag = |value: &Value| -> Value {
            match value.as_str() {
                Some(_) => value.with_tag("rule", Value::str(rule.name.as_str())),
                None => value.clone(),
            }
        };
        Ok(Some(match name {
            "output" => {
                let mut items = rule.output.clone();
                items.values = items.values.iter().map(tag).collect();
                NamedList::from_items("OutputFiles", &items).value()
            }
            "input" => NamedList::from_items("InputFiles", &rule.input).value(),
            "log" => NamedList::from_items("Log", &rule.log).value(),
            "params" => NamedList::from_items("Params", &rule.params).value(),
            "benchmark" => rule.benchmark.clone().unwrap_or(Value::None),
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
    /// The first output of each checkpoint that `get` found done, since the last `take`.
    static CONSULTED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}
