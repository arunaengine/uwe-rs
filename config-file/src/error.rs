//! What can be wrong with a config file: it cannot be read, or a value in it is wrong.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use crate::Origin;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the config file {file}: {source}")]
    Read {
        file: PathBuf,
        source: std::io::Error,
    },
    /// A value is wrong, missing or not allowed; the origin says where.
    #[error("{origin}: {message}")]
    Invalid { origin: Origin, message: String },
}

impl ConfigError {
    pub fn at(origin: &Origin, message: impl Into<String>) -> ConfigError {
        ConfigError::Invalid {
            origin: origin.clone(),
            message: message.into(),
        }
    }
}
