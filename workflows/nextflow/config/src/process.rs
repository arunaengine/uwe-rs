//! The `process` scope for one process, and the `agent` scope for one agent: the settings of
//! the matching selectors and the plain defaults (config spec, section 6).
// Copyright (c) 2026 The uwe Contributors
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
        self.scope_settings("process", names, labels, &[])
    }

    /// The settings of the `agent` scope for an agent, as for a process, without the options
    /// that hold for the whole run, like `agent.baseUrl`.
    pub fn agent_settings(
        &self,
        names: &[&str],
        labels: &[String],
    ) -> Result<ProcessSettings, Exception> {
        self.scope_settings("agent", names, labels, RUN_OPTIONS)
    }

    fn scope_settings(
        &self,
        scope: &str,
        names: &[&str],
        labels: &[String],
        left_out: &[&str],
    ) -> Result<ProcessSettings, Exception> {
        let mut settings = ProcessSettings::default();
        let Some(Value::Map(scope)) = self.get(&[scope]) else {
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
                let mut matched = false;
                for label in &labels {
                    if pattern.matches(label)? {
                        matched = true;
                        break;
                    }
                }
                if matched != negated {
                    add(&mut settings.selected, body, left_out);
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
                    if pattern.matches(name)? != negated {
                        add(&mut settings.selected, body, left_out);
                    }
                }
            }
        }
        for (key, value) in entries {
            if setting(&key, left_out) {
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

/// The settings of a selector body, without nested selectors, `params` and those left out.
fn add(settings: &mut Vec<(Rc<str>, Value)>, body: &Value, left_out: &[&str]) {
    let Value::Map(body) = body else {
        return;
    };
    for (key, value) in &body.borrow().entries {
        let key = key.0.to_string();
        if setting(&key, left_out) {
            settings.push((key.into(), value.clone()));
        }
    }
}

/// Whether a key of the scope sets a directive: not a selector, `params` or one left out.
fn setting(key: &str, left_out: &[&str]) -> bool {
    !key.starts_with("withLabel:")
        && !key.starts_with("withName:")
        && key != "params"
        && !left_out.contains(&key)
}

/// The options of the `agent` scope that hold for the whole run; `model` and
/// `maxIterations` are defaults of agent directives instead.
const RUN_OPTIONS: &[&str] = &[
    "runner",
    "apiProvider",
    "apiKey",
    "baseUrl",
    "maxToolOutputInlineSize",
    "requestTimeout",
    "trace",
    "rpc",
];
