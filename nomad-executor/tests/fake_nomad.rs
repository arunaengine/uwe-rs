//! Checks the Nomad backend against a local fake of the Nomad HTTP API that runs each job's
//! command here, and its job bodies against the requests of the real nf-nomad.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staging};
use nomad_executor::{Cleanup, Client, NomadBackend, Settings};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeNomad;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Settings like those of the fixture: one datacenter, the namespace `lab`, no cleanup.
fn settings(fake: &FakeNomad) -> Settings {
    Settings {
        address: Some(fake.address.clone()),
        namespace: Some("lab".into()),
        datacenters: vec!["dc1".into()],
        network_mode: Some("bridge".into()),
        cleanup: Cleanup::Never,
        poll: Duration::from_millis(50),
        ..Settings::default()
    }
}

/// A backend whose calls go to the fake; with `stage`, through a staging root in `folder`.
fn backend(folder: &Path, settings: Settings, stage: bool) -> NomadBackend {
    let address = settings.address.clone().unwrap_or_default();
    let mut client = Client::new(
        &address,
        Some("not-a-real-token".into()),
        Some("lab".into()),
        None,
    );
