//! The one HTTP client of the process for executors and the task runner: checks of images at
//! their registries, and requests to TES servers. It is the client of the storage crate, which
//! the storage providers share, so that the whole process reuses connections and looks up a
//! host's address once.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub use storage::http_client as client;
