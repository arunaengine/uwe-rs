//! Builds TES executors with configured staging, authentication and site routing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use storage::Storage;
use task_runner::{Runner, RunnerExecutor, Site, SiteRule, Sites};
use tes_executor::{Client, ObjectStage, Settings, TesBackend, TesError};

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::{Setup, TesSite};

/// The address of a TES server when `-tes-server` names none: Funnel's default.
pub const DEFAULT_SERVER: &str = "http://localhost:8000";

/// Where tasks run: the TES server, its bearer token when it needs one, more servers each near
/// some data, and the scratch directory of this machine.
pub struct Place<'a> {
    pub server: &'a str,
    pub token: Option<String>,
    pub settings: &'a Settings,
    pub sites: &'a [TesSite],
    pub scratch: &'a Path,
}

/// The TES server of `-tes-server`, else of the config file, else `DEFAULT_SERVER`, with its
/// token from the given `UWE_TES_TOKEN`, else from the config file.
pub fn place<'a>(
    arguments: &'a Arguments,
    setup: &'a Setup,
    scratch: &'a Path,
) -> Result<Place<'a>, Failure> {
    let settings = &setup.executors;
    let server = (arguments.tes_server.as_deref())
        .or(settings.tes_server.as_deref())
        .unwrap_or(DEFAULT_SERVER);
    let token = match arguments.secrets.get("UWE_TES_TOKEN") {
        Some(token) => Some(token),
        None => (settings.tes_token.as_ref().map(|secret| secret.reveal()))
            .transpose()
            .map_err(|error| Failure::setup(format!("uwe: {error}")))?,
    };
    Ok(Place {
        server,
        token,
        settings: &settings.tes,
        sites: &settings.tes_sites,
        scratch,
    })
}

/// An executor that runs tasks on the TES server of `place`, or on the server of the site that
/// is near most of their inputs. Tasks without an
/// image get `default_image`.
pub fn executor(
    store: Store,
    place: Place<'_>,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::executors::conda::Remote<Sites<TesBackend>>>, String> {
    let scratch = place.scratch.join("tes");
    let backends = backends(store.clone(), &place, run, &storage)?;
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    let peers: Vec<&TesBackend> = std::iter::once(&backends.default)
        .chain(backends.sites.values())
        .collect();
    runtime
        .block_on(backends.default.reconcile(&peers))
        .map_err(|error| error.to_string())?;
    // The server reads the inputs from the store, so they are copied there.
    let backends =
        crate::executors::conda::Remote::new(backends, store.clone(), &scratch, run, setup);
    let context = crate::run::RunContext {
        storage,
        ..setup.context()
    };
    let mut runner = Runner::new(store, backends, run, crate::runner_settings(&context, true));
    runner.references(setup.references.clone());
    let mut executor = RunnerExecutor::new(runner, default_image);
    executor.sites = SiteRule {
        sites: (place.sites.iter())
            .map(|site| Site {
                name: site.name.clone(),
                near: site.near.clone(),
                ..Site::default()
            })
            .collect(),
        ..SiteRule::default()
    };
    Ok(executor)
}

/// The backends of the run `run`: one for the TES server of `place` and one for each site.
pub(crate) fn backends(
    store: Store,
    place: &Place<'_>,
    run: &str,
    storage: &Storage,
) -> Result<Sites<TesBackend>, String> {
    let scratch = place.scratch.join("tes");
    let backend =
        |server: &str, token: Option<String>, settings: &Settings| -> Result<TesBackend, String> {
            // The server is reached as the storage of the run reaches the web.
            let client = settings
                .client(server, token)?
                .with_proxy(storage.proxy())?;
            answers(&client, server)?;
            let staging = settings
                .staging_root
                .as_ref()
                .map(|_| ObjectStage::configured(store.clone(), storage, settings))
                .transpose()?;
            Ok(TesBackend {
                run: run.into(),
                client,
                scratch: scratch.clone(),
                configs: Default::default(),
                settings: settings.clone(),
                staging,
            })
        };
    let mut backends = Sites::one(backend(place.server, place.token.clone(), place.settings)?);
    for site in place.sites {
        let token = (site.token.as_ref().map(|secret| secret.reveal()))
            .transpose()
            .map_err(|error| error.to_string())?;
        backends.sites.insert(
            site.name.clone(),
            backend(&site.server, token, &site.settings)?,
        );
    }
    Ok(backends)
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
             with UWE_TES_TOKEN or `token` in [executor.tes] of the config file"
        )),
        Err(TesError::Request { message, .. }) => Err(format!(
            "the TES server at {server} does not answer: {message}; start it, or name another \
             with -tes-server or `server` in [executor.tes] of the config file"
        )),
        Err(_) => Ok(()),
    }
}
