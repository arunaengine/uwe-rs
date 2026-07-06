//! Storage providers (decision 20). The config file maps address prefixes to providers, each
//! with its endpoint, region, credentials and settings: S3 and the services that speak its API,
//! Google Cloud Storage, Azure Blob Storage, web servers, FTP servers and local folders. The
//! longest prefix that serves an address wins. Web addresses, public buckets and local files
//! that no provider serves go to a built-in one, so that a machine without a config file reads
//! public files as before. Each provider makes its client once per process, when it is first
//! used; secrets never show in messages.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod address;
mod error;
pub mod settings;

pub use error::StorageError;
pub use settings::Settings;
