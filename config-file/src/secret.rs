//! Resolves written, given and environment secrets on use while hiding their printed values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::{ConfigError, Origin};

/// A secret setting.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    source: Source,
    origin: Origin,
}

#[derive(Clone, PartialEq, Eq)]
enum Source {
    /// The secret as written in the file.
    Written(String),
    /// The name of the environment variable that holds the secret, and the values that the
    /// host gave in its place.
    Environment(String, SecretValues),
}

impl Secret {
    /// A secret as written at `origin`, like a key of a Nextflow config.
    pub fn written(secret: String, origin: Origin) -> Secret {
        Secret {
            source: Source::Written(secret),
            origin,
        }
    }

    pub(crate) fn environment(name: String, given: SecretValues, origin: Origin) -> Secret {
        Secret {
            source: Source::Environment(name, given),
            origin,
        }
    }

    /// The secret itself: as written, else the value the host gave for its environment
    /// variable, else, when the host allows it, the variable, which must be set and not empty.
    pub fn reveal(&self) -> Result<String, ConfigError> {
        match &self.source {
            Source::Written(secret) => Ok(secret.clone()),
            Source::Environment(name, given) => {
                if let Some(value) = given.values.get(name) {
                    return Ok(value.clone());
                }
                if !given.reads(name) {
                    let message = format!("the host gave no value for the secret {name}");
                    return Err(ConfigError::at(&self.origin, message));
                }
                Self::from_environment(name, &self.origin)
            }
        }
    }

    fn from_environment(name: &str, origin: &Origin) -> Result<String, ConfigError> {
        match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) => Err(ConfigError::at(
                origin,
                format!("the environment variable {name}, which holds this secret, is empty"),
            )),
            Err(_) => Err(ConfigError::at(
                origin,
                format!("the environment variable {name}, which holds this secret, is not set"),
            )),
        }
    }

    /// Where the secret, or its reference, was written.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The environment variable that holds the secret, if it comes from one.
    pub fn variable(&self) -> Option<&str> {
        match &self.source {
            Source::Environment(name, _) => Some(name),
            Source::Written(_) => None,
        }
    }
}

/// Shows where the secret comes from, never the secret itself.
impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Source::Written(_) => write!(formatter, "Secret(written at {})", self.origin),
            Source::Environment(name, _) => write!(formatter, "Secret(${name})"),
        }
    }
}

/// The secret values a host gives for one request, by the name of the environment variable
/// that each stands for. Without a value for a name, the variable is read when allowed.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValues {
    values: Arc<BTreeMap<String, String>>,
    environment: bool,
    /// The only variables of the process that are read, when the host limits them.
    allowed: Option<Arc<BTreeSet<String>>>,
}

impl Default for SecretValues {
    /// No values, and the process environment allowed, as for the command line.
    fn default() -> Self {
        SecretValues {
            values: Arc::default(),
            environment: true,
            allowed: None,
        }
    }
}

impl SecretValues {
    /// Gives `value` for the environment variable `name`. A name must not be empty or hold
    /// `=` or a NUL byte; a value must not be empty or hold a NUL byte.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), ConfigError> {
        let (name, value) = (name.into(), value.into());
        let refused = |message: String| Err(ConfigError::Refused(message));
        if name.is_empty() {
            return refused("a secret needs a name".into());
        }
        if name.contains(['=', '\0']) {
            return refused(format!("the secret name {name:?} holds `=` or a NUL byte"));
        }
        if value.is_empty() {
            return refused(format!("the value of the secret {name} is empty"));
        }
        if value.contains('\0') {
            return refused(format!("the value of the secret {name} holds a NUL byte"));
        }
        Arc::make_mut(&mut self.values).insert(name, value);
        Ok(())
    }

    /// Whether a name without a given value is read from the process environment.
    pub fn allow_environment(&mut self, allowed: bool) {
        self.environment = allowed;
    }

    /// Reads only these variables of the process environment for a name without a given
    /// value, when the environment is allowed. Other names count as not set.
    pub fn limit_environment(&mut self, names: impl IntoIterator<Item = String>) {
        self.allowed = Some(Arc::new(names.into_iter().collect()));
    }

    /// Whether the variable `name` of the process may be read.
    fn reads(&self, name: &str) -> bool {
        self.environment
            && self
                .allowed
                .as_ref()
                .is_none_or(|names| names.contains(name))
    }

    /// The value for `name`: the given one, else its environment variable when allowed and
    /// not empty.
    pub fn get(&self, name: &str) -> Option<String> {
        if let Some(value) = self.values.get(name) {
            return Some(value.clone());
        }
        if !self.reads(name) {
            return None;
        }
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }

    /// The value of the variable `name` as a workflow reads its environment: the given one,
    /// else the variable of the process when allowed, also when it is empty.
    pub fn variable(&self, name: &str) -> Option<String> {
        if let Some(value) = self.values.get(name) {
            return Some(value.clone());
        }
        self.reads(name).then(|| std::env::var(name).ok()).flatten()
    }

    /// The environment as a workflow sees it: the variables of the process when allowed, in
    /// their order, with the given values in place of theirs or after them.
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut variables: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| self.reads(name))
            .collect();
        for (name, value) in self.values.iter() {
            match variables.iter_mut().find(|(known, _)| known == name) {
                Some(entry) => entry.1 = value.clone(),
                None => variables.push((name.clone(), value.clone())),
            }
        }
        variables
    }

    /// The names that have a given value, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

/// Shows the names and whether the environment is read, never the values.
impl fmt::Debug for SecretValues {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretValues")
            .field("names", &self.values.keys().collect::<Vec<_>>())
            .field("environment", &self.environment)
            .field("allowed", &self.allowed)
            .finish()
    }
}
