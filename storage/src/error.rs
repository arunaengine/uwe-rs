//! What can go wrong with storage, in words that never hold a secret.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use config_file::ConfigError;

/// What replaces a secret in a message.
const HIDDEN: &str = "[secret]";

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

/// Why a request of a provider failed, before the provider names the address and hides its
/// secrets.
#[derive(Debug)]
pub(crate) struct Problem {
    pub message: String,
    /// Whether another try may work, as after a network error.
    pub retryable: bool,
    /// Whether the file is not there.
    pub missing: bool,
}

impl Problem {
    /// A problem that another try does not fix.
    pub fn fixed(message: impl Into<String>) -> Problem {
        Problem {
            message: message.into(),
            retryable: false,
            missing: false,
        }
    }

    /// A problem that another try may fix, as after a network error.
    pub fn passing(message: impl Into<String>) -> Problem {
        Problem {
            retryable: true,
            ..Problem::fixed(message)
        }
    }

    /// The file is not there.
    pub fn missing() -> Problem {
        Problem {
            missing: true,
            ..Problem::fixed("it does not exist")
        }
    }

    /// A local file failed.
    pub fn local(path: &Path, error: std::io::Error) -> Problem {
        Problem::fixed(format!("{}: {error}", path.display()))
    }
}

/// An error of a local file at `path`.
pub(crate) fn local(path: &Path) -> impl Fn(std::io::Error) -> StorageError + '_ {
    move |source| StorageError::Local {
        path: path.to_owned(),
        source,
    }
}

/// A text with each of the secrets replaced, so that a message of a server or a library can
/// be shown.
pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret.as_str(), HIDDEN);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_hidden_in_messages() {
        let secrets = ["s3cr3t".to_owned(), String::new()];
        assert_eq!(
            redact("signature s3cr3t does not match s3cr3t", &secrets),
            "signature [secret] does not match [secret]"
        );
        assert_eq!(redact("nothing here", &secrets), "nothing here");
    }
}
