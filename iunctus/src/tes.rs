//! The TES executor: each task runs on a GA4GH TES server through the task runner. The store
//! and the scratch directories are on this machine, and the server reads and writes them at
//! the same paths through `file://` URLs, as Funnel on this machine or on a cluster with a
//! shared file system does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use storage::Storage;
use task_runner::{Runner, RunnerExecutor, Site, SiteRule, Sites};
use tes_executor::{Client, TesBackend, TesError};

use crate::setup::TesSite;

/// The address of a TES server when `-tes-server` names none: Funnel's default.
pub const DEFAULT_SERVER: &str = "http://localhost:8000";

/// Where tasks run: the TES server, its bearer token when it needs one, more servers each near
/// some data, and the scratch directory of this machine.
pub struct Place<'a> {
    pub server: &'a str,
    pub token: Option<String>,
    pub sites: &'a [TesSite],
    pub scratch: &'a Path,
}

/// An executor that runs tasks on the TES server of `place`, or on the server of the site that
/// is near most of their inputs (`docs/design/efficiency.md`, section 3). Tasks without an
/// image get `default_image`.
pub fn executor(
    store: Store,
    place: Place<'_>,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
) -> Result<RunnerExecutor<Sites<TesBackend>>, String> {
    let scratch = place.scratch.join("tes");
    let backend = |server: &str, token: Option<String>| -> Result<TesBackend, String> {
        let client = Client::new(server, token).map_err(|error| error.to_string())?;
        answers(&client, server)?;
        Ok(TesBackend {
            client,
            scratch: scratch.clone(),
            configs: Default::default(),
        })
    };
    let mut backends = Sites::one(backend(place.server, place.token)?);
    for site in place.sites {
        let token = (site.token.as_ref().map(|secret| secret.reveal()))
            .transpose()
            .map_err(|error| error.to_string())?;
        backends
            .sites
            .insert(site.name.clone(), backend(&site.server, token)?);
    }
    // The server reads the inputs from the store, so they are copied there.
    let runner = Runner::new(store, backends, run, crate::runner_settings(storage, true));
    let mut executor = RunnerExecutor::new(runner, default_image);
    executor.sites = SiteRule {
        sites: (place.sites.iter())
            .map(|site| Site {
                name: site.name.clone(),
                near: site.near.clone(),
            })
            .collect(),
    };
    Ok(executor)
}

/// Whether the server answers, asked once before the run, so that a server that is not there
/// stops the run at once rather than after the retries of its first task. A server that
/// answers with an error still answers, unless it refuses the token.
pub fn answers(client: &Client, server: &str) -> Result<(), String> {
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    match runtime.block_on(client.service_info()) {
        Ok(_) => Ok(()),
        Err(TesError::Status { status, .. }) if status == 401 || status == 403 => Err(format!(
            "the TES server at {server} refuses the request (status {status}); give its token \
             with IUNCTUS_TES_TOKEN or `token` in [executor.tes] of the config file"
        )),
        Err(TesError::Request { message, .. }) => Err(format!(
            "the TES server at {server} does not answer: {message}; start it, or name another \
             with -tes-server or `server` in [executor.tes] of the config file"
        )),
        Err(_) => Ok(()),
    }
}
