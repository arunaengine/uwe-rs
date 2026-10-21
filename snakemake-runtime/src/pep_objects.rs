//! The objects of a PEP project in the Snakefile: the `Project` and its `Sample`s.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use python_evaluator::{Error, Interpreter, Native, Value};

use super::Attributes;

/// `pep`: a peppy `Project`.
pub struct Project {
    pub(super) config: Value,
    pub(super) config_file: String,
    pub(super) index: String,
    pub(super) sample_table: Value,
    pub(super) subsample_table: Value,
    pub(super) samples: Vec<Value>,
}

impl Native for Project {
    fn type_name(&self) -> &str {
        "Project"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        Ok(Some(match name {
            "config" => self.config.clone(),
            "config_file" => Value::str(self.config_file.as_str()),
            "sample_table" => self.sample_table.clone(),
            "subsample_table" => self.subsample_table.clone(),
            "samples" => Value::list(self.samples.clone()),
            "sample_table_index" | "sample_name_colname" => Value::str(self.index.as_str()),
            "pep_version" => interpreter.get_item(&self.config, &Value::str("pep_version"))?,
            "name" | "description" => {
                let key = if name == "name" {
                    "name"
                } else {
                    "description"
                };
                match interpreter.get_item(&self.config, &Value::str(key)) {
                    Ok(value) => value,
                    Err(_) => Value::None,
                }
            }
            "get_sample" | "get_samples" => {
                let many = name == "get_samples";
                Value::builtin(name, move |interpreter, args| {
                    let Some(project) = this.as_native::<Project>() else {
                        return Err(Error::type_error("not a Project"));
                    };
                    let wanted = match args.get(0) {
                        Some(names) if many => interpreter.iterate(names)?,
                        Some(name) => vec![name.clone()],
                        None => Vec::new(),
                    };
                    let mut found = Vec::new();
                    for sample in &project.samples {
                        let Some(sample) = sample.as_native::<Sample>() else {
                            continue;
                        };
                        let name = sample.0.get(&project.index).cloned().unwrap_or(Value::None);
                        if wanted
                            .iter()
                            .any(|wanted| python_evaluator::value::key_equal(wanted, &name))
                        {
                            found.push(Value::native(Sample(sample.0.clone())));
                        }
                    }
                    if many {
                        return Ok(Value::list(found));
                    }
                    match found.into_iter().next() {
                        Some(sample) => Ok(sample),
                        None => Err(Error::value_error(format!(
                            "Project has no sample named {}.",
                            interpreter.to_str(&wanted.first().cloned().unwrap_or(Value::None))?
                        ))),
                    }
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// A peppy `Sample`: its attributes by name, as a mapping and as attributes.
pub struct Sample(pub(super) Attributes);

impl Sample {
    fn public(&self) -> Vec<(String, Value)> {
        self.0
            .iter()
            .filter(|(key, _)| !key.starts_with('_'))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

impl Native for Sample {
    fn type_name(&self) -> &str {
        "Sample"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        matches!(class, "Mapping" | "MutableMapping" | "dict")
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        if let Some(value) = self.0.get(name) {
            return Ok(Some(value.clone()));
        }
        let entries = self.public();
        Ok(Some(match name {
            "keys" => {
                let keys: Vec<Value> = entries
                    .iter()
                    .map(|(key, _)| Value::str(key.as_str()))
                    .collect();
                Value::builtin("keys", move |_, _| Ok(Value::list(keys.clone())))
            }
            "values" => {
                let values: Vec<Value> = entries.iter().map(|(_, value)| value.clone()).collect();
                Value::builtin("values", move |_, _| Ok(Value::list(values.clone())))
            }
            "items" => Value::builtin("items", move |_, _| {
                Ok(Value::list(
                    entries
                        .iter()
                        .map(|(key, value)| {
                            Value::tuple(vec![Value::str(key.as_str()), value.clone()])
                        })
                        .collect(),
                ))
            }),
            "to_dict" => Value::builtin("to_dict", move |_, _| Ok(Value::dict_of(entries.clone()))),
            "get" => {
                let all = self.0.clone();
                Value::builtin("get", move |interpreter, args| {
                    let key = interpreter.to_str(args.get(0).unwrap_or(&Value::None))?;
                    Ok(all
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::None)))
                })
            }
            _ => {
                return Err(Error::attribute_error(format!(
                    "'Sample' object has no attribute '{name}'"
                )));
            }
        }))
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let name = interpreter.to_str(key)?;
        match self.0.get(&name) {
            Some(value) => Ok(Some(value.clone())),
            None => Err(Error::key_error(key.clone())),
        }
    }

    fn contains(&self, interpreter: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        let name = interpreter.to_str(item)?;
        Ok(Some(self.0.contains_key(&name)))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(
            self.public()
                .into_iter()
                .map(|(key, _)| Value::str(key))
                .collect(),
        ))
    }

    fn length(&self) -> Option<usize> {
        Some(self.public().len())
    }
}
