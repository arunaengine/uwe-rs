//! What can go wrong with storage, in words that never hold a secret.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use config_file::ConfigError;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// No provider of the config file serves the address, and iunctus has none built in.
    #[error(
        "no storage provider serves {address}; name one in the config file, like \
         [storage.<name>] with its type and prefixes = [\"{prefix}\"]"
    )]
    NoProvider { address: String, prefix: String },
    /// A setting of the provider is wrong, or a secret is missing.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The provider cannot do what was asked, like writing to a web server.
    #[error("{0}")]
    Unsupported(String),
    /// The file is not there.
    #[error("{address} does not exist ({provider})")]
    Missing { address: String, provider: String },
    /// The provider failed.
    #[error("cannot {action} {address} ({provider}): {message}")]
    Failed {
        action: &'static str,
        address: String,
        provider: String,
        message: String,
        /// Whether another try may work, as after a network error.
        retryable: bool,
    },
    /// A local file failed.
    #[error("{}: {source}", path.display())]
    Local {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl StorageError {
    /// Whether another try may work.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            StorageError::Failed {
                retryable: true,
                ..
            }
        )
    }

    /// Whether iunctus cannot do this at all, rather than failed to.
    pub fn unsupported(&self) -> bool {
        matches!(
            self,
            StorageError::Unsupported(_) | StorageError::NoProvider { .. }
        )
    }
}
