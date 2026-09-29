//! Resolves written and environment secrets on use while hiding their printed values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

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
    /// The name of the environment variable that holds the secret.
    Environment(String),
}

impl Secret {
    pub(crate) fn written(secret: String, origin: Origin) -> Secret {
        Secret {
            source: Source::Written(secret),
            origin,
        }
    }

    pub(crate) fn environment(name: String, origin: Origin) -> Secret {
        Secret {
            source: Source::Environment(name),
            origin,
        }
    }

    /// The secret itself: as written, or the value of its environment variable, which must be
    /// set and not empty.
    pub fn reveal(&self) -> Result<String, ConfigError> {
        match &self.source {
            Source::Written(secret) => Ok(secret.clone()),
            Source::Environment(name) => match std::env::var(name) {
                Ok(value) if !value.is_empty() => Ok(value),
                Ok(_) => Err(ConfigError::at(
                    &self.origin,
                    format!("the environment variable {name}, which holds this secret, is empty"),
                )),
                Err(_) => Err(ConfigError::at(
                    &self.origin,
                    format!("the environment variable {name}, which holds this secret, is not set"),
                )),
            },
        }
    }

    /// Where the secret, or its reference, was written.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The environment variable that holds the secret, if it comes from one.
    pub fn variable(&self) -> Option<&str> {
        match &self.source {
            Source::Environment(name) => Some(name),
            Source::Written(_) => None,
        }
    }
}

/// Shows where the secret comes from, never the secret itself.
impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Source::Written(_) => write!(formatter, "Secret(written at {})", self.origin),
            Source::Environment(name) => write!(formatter, "Secret(${name})"),
        }
    }
}
