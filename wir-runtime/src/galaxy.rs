//! Galaxy native blocks: the Galaxy driver of uwe as a child, with the op inputs as the job of
//! the tool or workflow, and its outputs as Files by output name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use galaxy_runtime::{GalaxyDriver, Options};
use serde_json::Value as Json;
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// A Galaxy driver for the launch of one block. A child never prints its outputs.
pub(crate) fn block(launch: &Launch, options: &Options) -> Result<Box<dyn Block>, DriverError> {
    let options = Options {
        print: false,
        ..options.clone()
    };
    Ok(Box::new(GalaxyDriver::new(launch, options)?))
}

impl Block for GalaxyDriver {
    /// The outputs of the tool or workflow, which a simulated run gives too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(GalaxyDriver::exported(self))
    }
}
