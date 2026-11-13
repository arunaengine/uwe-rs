//! Expands path variables with rule, module, config, workflow, and default precedence.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use indexmap::IndexMap;
use python_evaluator::{Error, Interpreter, Native, Value};

use crate::workflow::workflow_error;

/// Where a path variable was set; a lower level wins over a higher one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Rule = 0,
    Module = 1,
    Config = 2,
    Workflow = 3,
    Default = 4,
}

#[derive(Clone, Default)]
pub struct Pathvars {
    items: IndexMap<String, (String, Level)>,
}

/// Whether a name can be a path variable: lower-case letters, digits and `_`, starting with
/// a letter.
fn valid_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Pathvars {
    /// The variables that every workflow has.
    pub fn defaults() -> Pathvars {
        let mut pathvars = Pathvars::default();
        for name in [
            "results",
            "stats",
            "reports",
            "temp",
            "resources",
            "logs",
            "benchmarks",
        ] {
            pathvars
                .items
                .insert(name.to_owned(), (name.to_owned(), Level::Default));
        }
        pathvars
    }

    /// Variables given as names and texts, like the arguments of a `pathvars:` directive.
    pub fn from_pairs(
        interpreter: &mut Interpreter,
        pairs: &[(Value, Value)],
        level: Level,
    ) -> Result<Pathvars, Error> {
        let mut items = IndexMap::new();
        let mut invalid = Vec::new();
        for (name, value) in pairs {
            match (name.as_str(), value.as_str()) {
                (Some(name), Some(value)) if valid_name(name) => {
                    items.insert(name.to_owned(), (value.to_owned(), level));
                }
                _ => invalid.push(format!(
                    "{}: {}",
                    interpreter.repr(name)?,
                    interpreter.repr(value)?
                )),
            }
        }
        if !invalid.is_empty() {
            return Err(workflow_error(format!(
                "Pathvars have to be a mapping of str to str, with keys being valid pathvar names (i.e. alphanumeric + _, lower-case, no leading number). The following entries are invalid: {}",
                invalid.join(", ")
            )));
        }
        let pathvars = Pathvars { items };
        pathvars.check()?;
        Ok(pathvars)
    }

    /// The variables of the `pathvars` entry of a config.
    pub fn from_config(
        interpreter: &mut Interpreter,
        config: &Value,
        level: Level,
    ) -> Result<Pathvars, Error> {
        let Value::Dict(dict) = config.plain() else {
            return Ok(Pathvars::default());
        };
        let entry = dict.borrow().get("pathvars").cloned();
        match entry {
            Some(entry) if interpreter.truthy(&entry)? => {
                let pairs = interpreter.mapping_items(&entry)?;
                Pathvars::from_pairs(interpreter, &pairs, level)
            }
            _ => Ok(Pathvars::default()),
        }
    }

    /// Takes the variables of `other` that are set at the same or a narrower place.
    pub fn update(&mut self, other: &Pathvars) -> Result<(), Error> {
        for (name, (value, level)) in &other.items {
            let replace = self
                .items
                .get(name)
                .is_none_or(|(_, existing)| level <= existing);
            if replace {
                self.items.insert(name.clone(), (value.clone(), *level));
            }
        }
        self.check()
    }

    /// Fails when variables refer to each other in a cycle, which could not be expanded.
    fn check(&self) -> Result<(), Error> {
        fn visit(pathvars: &Pathvars, name: &str, seen: &mut Vec<String>) -> Result<(), Error> {
            if seen.iter().any(|known| known == name) {
                seen.push(name.to_owned());
                return Err(workflow_error(format!(
                    "Cyclic pathvar reference detected between: {}",
                    seen.join(", ")
                )));
            }
            seen.push(name.to_owned());
            if let Some((value, _)) = pathvars.items.get(name) {
                for reference in references(value) {
                    if pathvars.items.contains_key(&reference) {
                        visit(pathvars, &reference, seen)?;
                    }
                }
            }
            seen.pop();
            Ok(())
        }
        for name in self.items.keys() {
            visit(self, name, &mut Vec::new())?;
        }
        Ok(())
    }

    /// The path with every variable replaced by its value, until none is left.
    pub fn apply(&self, path: &str) -> Result<String, Error> {
        let mut applied = path.to_owned();
        while let Some((start, end, name)) = first_reference(&applied) {
            let Some((value, _)) = self.items.get(&name) else {
                let intermediate = if applied == path {
                    ".".to_owned()
                } else {
                    format!(" (intermediate path: {applied}).")
                };
                return Err(workflow_error(format!(
                    "Undefined pathvar '{name}' when expanding pathvars in {path}{intermediate}"
                )));
            };
            applied.replace_range(start..end, value);
        }
        Ok(applied)
    }
}

/// `workflow.pathvars`: the path variables of the code that runs, with `apply` and `get`.
pub struct PathvarsObject(pub Pathvars);

impl Native for PathvarsObject {
    fn type_name(&self) -> &str {
        "Pathvars"
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
        let pathvars = self.0.clone();
        Ok(Some(match name {
            "apply" => Value::builtin("apply", move |interpreter, args| {
                let path = args.get(0).cloned().unwrap_or(Value::None);
                let path = interpreter.to_str(&path)?;
                Ok(Value::str(pathvars.apply(&path)?))
            }),
            "get" => Value::builtin("get", move |interpreter, args| {
                let name = args.get(0).cloned().unwrap_or(Value::None);
                let name = interpreter.to_str(&name)?;
                match pathvars.items.get(&name) {
                    Some((value, _)) => Ok(Value::str(value.as_str())),
                    None => Err(Error::key_error(Value::str(name))),
                }
            }),
            _ => return Ok(None),
        }))
    }
}

/// The first `<name>` in a text: its start, its end and the name.
fn first_reference(text: &str) -> Option<(usize, usize, String)> {
    let mut search = 0;
    while let Some(offset) = text[search..].find('<') {
        let start = search + offset;
        let rest = &text[start + 1..];
        if let Some(close) = rest.find('>') {
            let name = &rest[..close];
            if valid_name(name) {
                return Some((start, start + close + 2, name.to_owned()));
            }
        }
        search = start + 1;
    }
    None
}

/// The names of all `<name>` references in a text.
fn references(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some((_, end, name)) = first_reference(rest) {
        names.push(name);
        rest = &rest[end..];
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pathvars(pairs: &[(&str, &str)], level: Level) -> Pathvars {
        let mut pathvars = Pathvars::default();
        for (name, value) in pairs {
            pathvars
                .items
                .insert((*name).to_owned(), ((*value).to_owned(), level));
        }
        pathvars
    }

    #[test]
    fn expands_nested_variables() {
        let mut all = Pathvars::defaults();
        let workflow = pathvars(
            &[
                ("results", "results/custom"),
                ("per", "{sample}"),
                ("nested", "<per>"),
            ],
            Level::Workflow,
        );
        all.update(&workflow).expect("no cycle");
        assert_eq!(
            all.apply("<results>/b/<nested>.txt").expect("defined"),
            "results/custom/b/{sample}.txt"
        );
        assert_eq!(all.apply("<resources>/x").expect("defined"), "resources/x");
        assert_eq!(all.apply("a<b").expect("no variable"), "a<b");
    }

    #[test]
    fn narrower_places_win() {
        let mut rule = pathvars(&[("results", "rule")], Level::Rule);
        rule.update(&pathvars(&[("results", "workflow")], Level::Workflow))
            .expect("no cycle");
        assert_eq!(rule.apply("<results>").expect("defined"), "rule");
    }

    #[test]
    fn rejects_invalid_variables() {
        let error = Pathvars::defaults()
            .apply("<nothing>/x")
            .expect_err("undefined");
        assert!(
            error
                .to_string()
                .contains("Undefined pathvar 'nothing' when expanding pathvars in <nothing>/x."),
            "{error}"
        );
        let mut all = Pathvars::default();
        let cycle = pathvars(&[("a", "<b>"), ("b", "<a>")], Level::Workflow);
        assert!(all.update(&cycle).is_err());
    }
}
