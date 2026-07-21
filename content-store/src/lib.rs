//! The cache directory of iunctus on a local file system or in S3: contents, entries, outputs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod layout;
mod local;
mod objects;
mod store;

pub use objects::{FileIdentity, Object};
pub use store::{S3Options, Store, StoreError, StoredOutput, hash_file};
