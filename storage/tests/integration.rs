//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/service.rs"]
mod service;

#[path = "cas.rs"]
mod cas;
#[path = "drs.rs"]
mod drs;
#[path = "hosted.rs"]
mod hosted;
#[path = "irods.rs"]
mod irods;
#[path = "lakefs.rs"]
mod lakefs;
#[path = "lamin.rs"]
mod lamin;
#[path = "oci.rs"]
mod oci;
#[path = "providers.rs"]
mod providers;
#[path = "proxy.rs"]
mod proxy;
#[path = "quilt.rs"]
mod quilt;
#[path = "s3.rs"]
mod s3;
#[path = "synapse.rs"]
mod synapse;
