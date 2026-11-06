//! nf-wave: asks the Wave service for the images of tasks, when the `wave` scope turns it on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod request;
mod settings;

use nextflow_config::Config;

use settings::Settings;

/// What uwe says once when `wave.enabled` alone would send every image to the service.
const LOCAL: &str = "Wave is not contacted: uwe builds Conda images and pulls images itself; \
                     set wave.endpoint, wave.freeze, wave.mirror, wave.scan.mode or \
                     wave.build.repository to use the Wave service";

/// What a task gives Wave to make its image from.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub process: String,
    /// The `container` directive, with the registry of the engine.
    pub container: Option<String>,
    pub conda: Option<String>,
    /// The `arch` directive, like `linux/arm64`.
    pub platform: Option<String>,
    /// The Conda channels of the config, for package lists.
    pub channels: Vec<String>,
}

/// The Wave settings of a run.
pub struct Wave {
    settings: Settings,
    /// Whether the run only simulates tasks, so that Wave builds nothing.
    dry_run: bool,
}

/// The Wave settings of a config; `None` unless `wave.enabled` is set and the environment
/// variable `NXF_DISABLE_WAVE_SERVICE` is not.
pub fn wave(
    config: &Config,
    variable: &dyn Fn(&str) -> Option<String>,
    simulated: bool,
) -> Result<Option<Wave>, String> {
    let settings = settings::read(config, variable)?;
    Ok(settings.map(|settings| Wave {
        settings,
        dry_run: simulated,
    }))
}

impl Wave {
    /// What uwe says once when it keeps its own images although `wave.enabled` is set.
    pub fn note(&self) -> Option<&'static str> {
        (!self.settings.service).then_some(LOCAL)
    }
}

#[cfg(test)]
mod tests;
