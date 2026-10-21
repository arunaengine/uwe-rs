//! The cache directory of iunctus on a local file system or in S3: contents, entries, outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod layers;
pub mod layout;
mod lease;
mod local;
mod objects;
mod store;
mod transfer;

pub use lease::{Holder, LEASE_TIME, Lease};
pub use objects::{FileIdentity, Object};
pub use store::{
    EntrySnapshot, S3Options, Store, StoreError, StoredDirectory, StoredOutput, hash_file,
};
pub use transfer::Presigned;
