//! Content digests, written as `sha256:<hex>` so that the algorithm is part of the value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

const SHA256_PREFIX: &str = "sha256:";

/// The digest of some bytes. Only SHA-256 exists today; the prefix leaves room for others.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DigestError {
    #[error("unknown digest algorithm in `{0}`; expected `sha256:`")]
    Algorithm(String),
    #[error("`{0}` is not 64 lowercase hexadecimal digits")]
    Hex(String),
}

impl Digest {
    pub fn sha256(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self::from_sha256(&hasher.finalize())
    }

    /// Wraps a finished SHA-256 hash, for callers that hash streams themselves.
    pub fn from_sha256(hash: &[u8]) -> Self {
        let mut text = String::with_capacity(SHA256_PREFIX.len() + 64);
        text.push_str(SHA256_PREFIX);
        for byte in hash {
            text.push_str(&format!("{byte:02x}"));
        }
        Self(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The hexadecimal part without the algorithm prefix.
    pub fn hex(&self) -> &str {
        &self.0[SHA256_PREFIX.len()..]
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some(hex) = text.strip_prefix(SHA256_PREFIX) else {
            return Err(DigestError::Algorithm(text.to_owned()));
        };
        let valid = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return Err(DigestError::Hex(text.to_owned()));
        }
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
