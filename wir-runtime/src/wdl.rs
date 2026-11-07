//! WDL native blocks: the WDL driver of uwe as a child, with the op inputs as the input object
//! of the workflow, and its outputs as op outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;
use wdl_runtime::{Options, WdlDriver};
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// A WDL driver for the launch of one block. A child never prints its outputs.
pub(crate) fn block(launch: &Launch, options: &Options) -> Result<Box<dyn Block>, DriverError> {
    let options = Options {
        print: false,
        ..options.clone()
    };
    Ok(Box::new(WdlDriver::new(launch, options)?))
}

impl Block for WdlDriver {
    /// The outputs of the workflow, which a simulated run gives too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(WdlDriver::exported(self))
    }
}
