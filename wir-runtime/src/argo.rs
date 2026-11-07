//! Argo native blocks: the Argo driver of uwe as a child, with the op inputs as workflow
//! parameters, and the outputs of the workflow as op outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// An Argo driver for the launch of one block, with the services of the run.
pub(crate) fn block(
    launch: &Launch,
    services: &argo_runtime::Services,
) -> Result<Box<dyn Block>, DriverError> {
    let driver = argo_runtime::load_driver(launch, services.clone())?;
    Ok(Box::new(driver))
}

impl Block for argo_runtime::ArgoDriver {
    /// The outputs of the entrypoint and the global outputs, which a simulated run gives too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(argo_runtime::ArgoDriver::exported(self))
    }
}
