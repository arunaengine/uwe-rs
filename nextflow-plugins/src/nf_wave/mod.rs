//! nf-wave: asks the Wave service for the images of tasks, when the `wave` scope turns it on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod request;
mod settings;

use std::collections::HashMap;
use std::time::{Duration, Instant};

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

/// The Wave settings of a run, and the images that Wave gave.
pub struct Wave {
    settings: Settings,
    /// Whether the run only simulates tasks, so that Wave builds nothing.
    dry_run: bool,
    /// The target image of each request fingerprint, with the time Wave gave it.
    answers: HashMap<String, (Instant, String)>,
    runtime: Option<tokio::runtime::Runtime>,
    /// When the last request went out, for `httpClient.maxRate`.
    sent: Option<Instant>,
    /// The pause between status requests while Wave builds.
    pub poll: Duration,
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
        answers: HashMap::new(),
        runtime: None,
        sent: None,
        poll: Duration::from_secs(1),
    }))
}

impl Wave {
    /// What uwe says once when it keeps its own images although `wave.enabled` is set.
    pub fn note(&self) -> Option<&'static str> {
        (!self.settings.service).then_some(LOCAL)
    }

    /// The image that Wave made for the task, ready to pull; `None` when uwe keeps its own
    /// image, or the task has nothing for Wave.
    pub fn resolve(&mut self, sources: &Sources) -> Result<Option<String>, String> {
        if !self.settings.service {
            return Ok(None);
        }
        let Some(request) = self.request(sources)? else {
            return Ok(None);
        };
        let fingerprint = request["fingerprint"].as_str().unwrap_or_default();
        if let Some((given, target)) = self.answers.get(fingerprint)
            && given.elapsed() < self.settings.reuse
        {
            return Ok(Some(target.clone()));
        }
        let answer = self.submit(&request)?;
        let target = answer["targetImage"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if target.is_empty() {
            return Err(format!(
                "Wave invalid response: POST {}/v1alpha2/container - no target image",
                crate::service::safe_url(&self.settings.endpoint)
            ));
        }
        if !self.dry_run {
            self.ready(&answer, &target)?;
        }
        let fingerprint = fingerprint.to_owned();
        self.answers
            .insert(fingerprint, (Instant::now(), target.clone()));
        Ok(Some(target))
    }
}

#[cfg(test)]
mod tests;
