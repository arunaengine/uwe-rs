//! Snakemake native blocks: the Snakemake driver of uwe as a child that starts in a folder of
//! its own, with the op inputs as config, and its target files as op outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value as Json;
use snakemake_runtime::{RunSettings, Settings, SnakemakeDriver};
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// What the drivers of Snakemake blocks use, as a Snakemake run alone does.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub settings: Settings,
    /// The options of a run; each block starts in a folder of its own instead of `workdir`.
    pub run: RunSettings,
}

/// A Snakemake driver for the launch of one block, which starts in `folder`.
pub(crate) fn block(
    launch: &Launch,
    folder: &Path,
    options: &Options,
) -> Result<Box<dyn Block>, DriverError> {
    let run = RunSettings {
        workdir: Some(folder.to_owned()),
        ..options.run.clone()
    };
    let settings = options.settings.clone();
    Ok(Box::new(snakemake_runtime::spawn_driver(
        launch, settings, run,
    )?))
}

impl Block for SnakemakeDriver {
    /// The files of the targets, which a simulated run names too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(SnakemakeDriver::exported(self))
    }
}
