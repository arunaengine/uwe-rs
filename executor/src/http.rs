//! The one HTTP client of the process for executors and the task runner: downloads of remote
//! inputs, checks of images at their registries, and requests to TES servers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::OnceLock;
use std::time::Duration;

/// The client, made once per process and shared, so that requests to the same host reuse
/// their connection and do not look up the host's address again. Requests that must end in
/// time set their own timeout; a download may take as long as its data keeps coming.
pub fn client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("iunctus/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(300))
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
