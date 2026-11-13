//! nf-labrobotics 0.1.0: `labPlate`, and the environment that the `labrobotics` executor gives
//! the tasks of wet lab steps, which run with uwe's executor.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, TaskDirective, parse_json};
use nextflow_evaluator::units::parse_duration;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::{entries, get};
use crate::nf_sqldb::thrown;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["labPlate"],
    call: Some(call),
    scopes: &["labrobotics"],
    ..Plugin::new("nf-labrobotics", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    if name != "labPlate" {
        return Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-labrobotics`"),
            span: Default::default(),
        });
    }
    let mut plate = Vec::new();
    for file in arguments
        .iter()
        .take(2)
        .filter(|file| !matches!(file, Value::Null))
    {
        let path = crate::nf_schema::path(file, &context.host.launch_dir());
        let bytes = context.host.read(&path)?;
        let value = parse_json(&String::from_utf8_lossy(&bytes))
            .map_err(|error| thrown(error.message.to_string()))?;
        for (key, value) in entries(&value) {
            match plate
                .iter_mut()
                .find(|(known, _): &&mut (String, Value)| *known == key)
            {
                Some(entry) => entry.1 = value,
                None => plate.push((key, value)),
            }
        }
    }
    Ok(Value::map(
        plate
            .into_iter()
            .map(|(key, value)| (Value::string(key), value)),
    ))
}

/// What the `labrobotics` executor adds to the environment of a task.
#[derive(Clone, Debug)]
pub struct Lab {
    /// `NF_LAB_ADAPTER`, `NF_LAB_POLL` and `NF_LAB_GRACE` of the config, and `NF_LAB_SESSION`.
    fixed: BTreeMap<String, String>,
}

impl Lab {
    /// The settings of the `labrobotics` scope for a run with this session id.
    pub fn of(config: &Config, session: &str) -> Lab {
        let mut fixed = BTreeMap::new();
        let setting = |name: &str| {
            config
                .get(&["labrobotics", name])
                .filter(|value| value.truth())
        };
        if let Some(adapter) = setting("adapter") {
            fixed.insert("NF_LAB_ADAPTER".to_owned(), adapter.to_string());
        }
        for (name, variable) in [("poll", "NF_LAB_POLL"), ("grace", "NF_LAB_GRACE")] {
            let millis = setting(name).and_then(|value| match value {
                Value::Duration(millis) => Some(millis),
                other => parse_duration(&other.to_string()).ok(),
            });
            if let Some(millis) = millis {
                fixed.insert(variable.to_owned(), seconds(millis));
            }
        }
        fixed.insert("NF_LAB_SESSION".to_owned(), session.to_owned());
        Lab { fixed }
    }

    /// Adds the variables to a script task of the `labrobotics` executor: the capability of its
    /// `hints` and its `queue`, each removed when the task has none.
    pub fn apply(&self, directives: &[TaskDirective], environment: &mut BTreeMap<String, String>) {
        let last = |name: &str| {
            let found = directives
                .iter()
                .rev()
                .find(|directive| &*directive.name == name);
            found
                .and_then(|directive| directive.arguments.last())
                .filter(|value| value.truth())
        };
        if last("executor").map(|value| value.to_string()).as_deref() != Some("labrobotics") {
            return;
        }
        environment.extend(self.fixed.clone());
        let capability = last("hints").and_then(|hints| get(hints, "labrobotics/capability"));
        let found = [
            (
                "NF_LAB_CAPABILITY",
                capability.filter(|value| !matches!(value, Value::Null)),
            ),
            ("NF_LAB_QUEUE", last("queue").cloned()),
        ];
        for (name, value) in found {
            match value
                .map(|value| value.to_string())
                .filter(|text| !text.is_empty())
            {
                Some(text) => environment.insert(name.to_owned(), text),
                None => environment.remove(name),
            };
        }
    }
}

/// Milliseconds as seconds in Groovy's decimal division: `2`, `1.5` or `300`.
fn seconds(millis: i64) -> String {
    let whole = millis / 1000;
    match millis % 1000 {
        0 => whole.to_string(),
        rest => format!(
            "{whole}.{}",
            format!("{:03}", rest.abs()).trim_end_matches('0')
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_groovy_seconds() {
        assert_eq!(seconds(2000), "2");
        assert_eq!(seconds(1500), "1.5");
        assert_eq!(seconds(300_000), "300");
    }

    #[test]
    fn adds_task_environment() {
        let lab = Lab {
            fixed: BTreeMap::from([("NF_LAB_SESSION".to_owned(), "run".to_owned())]),
        };
        let directive = |name: &str, value: Value| TaskDirective {
            name: name.into(),
            arguments: vec![value],
        };
        let hints = Value::map([(
            Value::string("labrobotics/capability"),
            Value::string("liquid-handling"),
        )]);
        let mut environment = BTreeMap::from([("NF_LAB_QUEUE".to_owned(), "old".to_owned())]);
        lab.apply(
            &[
                directive("executor", Value::string("labrobotics")),
                directive("hints", hints),
            ],
            &mut environment,
        );
        assert_eq!(
            environment.get("NF_LAB_CAPABILITY").map(String::as_str),
            Some("liquid-handling")
        );
        assert_eq!(
            environment.get("NF_LAB_SESSION").map(String::as_str),
            Some("run")
        );
        assert_eq!(
            environment.get("NF_LAB_QUEUE"),
            None,
            "a missing queue is removed"
        );
        let mut other = BTreeMap::new();
        lab.apply(&[directive("executor", Value::string("local"))], &mut other);
        assert!(other.is_empty());
    }
}
