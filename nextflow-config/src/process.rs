//! The `process` scope for one process: the settings of the matching selectors and the plain
//! defaults (config spec, section 6).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::Value;

use crate::Config;

/// The config settings of one process.
#[derive(Clone, Debug, Default)]
pub struct ProcessSettings {
    /// The settings of the matching selectors, in the order they apply, so a later one wins:
    /// `withLabel` selectors, then `withName` selectors for the definition name, the alias and
    /// the fully qualified name.
    pub selected: Vec<(Rc<str>, Value)>,
    /// The plain `process` settings. Each fills in a directive that the process has no value
    /// for, or whose value is Nextflow's built-in default.
    pub defaults: Vec<(Rc<str>, Value)>,
}

impl Config {
    /// The settings for a process known by `names` (definition name, alias, fully qualified
    /// name) with the given labels. A selector with an invalid pattern is an error.
    pub fn process_settings(
        &self,
        names: &[&str],
        labels: &[String],
    ) -> Result<ProcessSettings, Exception> {
        let mut settings = ProcessSettings::default();
        let Some(Value::Map(scope)) = self.get(&["process"]) else {
            return Ok(settings);
        };
        let entries: Vec<(String, Value)> = scope
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| (key.0.to_string(), value.clone()))
            .collect();
        // A process without labels is matched as having the one label "".
        let labels: Vec<&str> = match labels.is_empty() {
            true => vec![""],
            false => labels.iter().map(String::as_str).collect(),
        };
        for (key, body) in &entries {
            if let Some(pattern) = key.strip_prefix("withLabel:") {
                let (pattern, negated) = selector(pattern)?;
                if labels.iter().any(|label| pattern.matches(label)) != negated {
                    add(&mut settings.selected, body);
                }
            }
        }
        let mut distinct: Vec<&str> = Vec::new();
        for name in names {
            if !name.is_empty() && !distinct.contains(name) {
                distinct.push(name);
            }
        }
        for name in distinct {
            for (key, body) in &entries {
                if let Some(pattern) = key.strip_prefix("withName:") {
                    let (pattern, negated) = selector(pattern)?;
                    if pattern.matches(name) != negated {
                        add(&mut settings.selected, body);
                    }
                }
            }
        }
        for (key, value) in entries {
            if !key.starts_with("withLabel:") && !key.starts_with("withName:") && key != "params" {
                settings.defaults.push((key.into(), value));
            }
        }
        Ok(settings)
    }
}

/// A selector pattern: a Java regular expression that must match a whole name, negated by a
/// leading `!`.
fn selector(text: &str) -> Result<(Pattern, bool), Exception> {
    let text = text.trim();
    let (text, negated) = match text.strip_prefix('!') {
        Some(rest) => (rest.trim(), true),
        None => (text, false),
    };
    Ok((Pattern::new(text)?, negated))
}

/// The settings of a selector body, without nested selectors and `params`.
fn add(settings: &mut Vec<(Rc<str>, Value)>, body: &Value) {
    let Value::Map(body) = body else {
        return;
    };
    for (key, value) in &body.borrow().entries {
        let key = key.0.to_string();
        if !key.starts_with("withLabel:") && !key.starts_with("withName:") && key != "params" {
            settings.push((key.into(), value.clone()));
        }
    }
}
