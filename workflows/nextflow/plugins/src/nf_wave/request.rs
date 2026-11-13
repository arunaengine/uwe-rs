//! Builds the body of the request to Wave for the image of a task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use base64::Engine;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use super::{Sources, Wave};

impl Wave {
    /// The body of the Wave request for a task, by the first source of `wave.strategy` it
    /// has; `None` when it has none.
    pub fn request(&self, sources: &Sources) -> Result<Option<Json>, String> {
        let mut image = None;
        let mut packages = None;
        for kind in &self.settings.strategy {
            match kind.as_str() {
                "container" if sources.container.is_some() => {
                    image = sources.container.clone();
                    break;
                }
                "conda" if sources.conda.is_some() => {
                    let conda = sources.conda.as_deref().unwrap_or_default();
                    packages = Some(self.packages(conda, &sources.channels)?);
                    break;
                }
                _ => {}
            }
        }
        if image.is_none() && packages.is_none() {
            return Ok(None);
        }
        if let Some(name) = image.as_deref().filter(|name| name.contains("://")) {
            return Err(format!(
                "Wave container request image cannot start with URL like prefix - offending value: {name}"
            ));
        }
        let settings = &self.settings;
        if settings.mirror && image.is_none() {
            return Err("Invalid container mirror operation - missing source container".into());
        }
        let platform = platform(sources.platform.as_deref());
        let sources = json!([image, packages, platform, settings.freeze, settings.mirror]);
        let digest = Sha256::digest(sources.to_string());
        let fingerprint: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let workspace = (settings.tower.workspace.as_deref())
            .map(|id| id.parse::<i64>().map_or(json!(id), |id| json!(id)));
        Ok(Some(json!({
            "containerImage": image,
            "containerFile": null,
            "packages": packages,
            "containerConfig": {
                "entrypoint": null, "cmd": null, "env": null, "workingDir": null, "layers": null
            },
            "containerPlatform": platform,
            "buildRepository": settings.repository,
            "cacheRepository": settings.cache_repository,
            "buildTemplate": settings.template,
            "buildCompression": settings.compression,
            "freeze": settings.freeze,
            "mirror": settings.mirror,
            "scanMode": settings.scan_mode,
            "scanLevels": settings.scan_levels,
            "format": null,
            "dryRun": self.dry_run,
            "fingerprint": fingerprint,
            "timestamp": timestamp(),
            "towerAccessToken": settings.tower.token,
            "towerRefreshToken": settings.tower.refresh,
            "towerWorkspaceId": workspace,
            "towerEndpoint": settings.tower.endpoint,
            "workflowId": null,
            "containerIncludes": null,
            "nameStrategy": null,
        })))
    }

    /// The Conda packages of a `conda` directive: an address, a local environment file sent
    /// as its base64 content, or a list of packages.
    fn packages(&self, conda: &str, channels: &[String]) -> Result<Json, String> {
        let web = conda.starts_with("http://") || conda.starts_with("https://");
        let (entries, environment, channels) = if web {
            (Some(vec![conda.to_owned()]), None, Some(channels))
        } else if local_file(conda) {
            let content = std::fs::read(conda).map_err(|error| format!("{conda}: {error}"))?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(content);
            (None, Some(encoded), None)
        } else {
            let entries = conda.split_whitespace().map(str::to_owned).collect();
            (Some(entries), None, Some(channels))
        };
        Ok(json!({
            "type": "CONDA",
            "entries": entries,
            "environment": environment,
            "channels": channels,
            "condaOpts": self.settings.conda,
            "pixiOpts": null,
            "cranOpts": null,
        }))
    }
}

/// Whether a `conda` directive names a local environment file rather than packages.
fn local_file(conda: &str) -> bool {
    let single = !conda.contains(char::is_whitespace);
    let named = [".yaml", ".yml", ".txt"]
        .iter()
        .any(|end| conda.ends_with(end));
    conda.starts_with('/') || single && named
}

/// The platform of an `arch` directive as Wave names it, `linux/amd64` by default.
fn platform(arch: Option<&str>) -> String {
    let arch = arch.unwrap_or("linux/amd64");
    let (system, machine) = arch.split_once('/').unwrap_or(("linux", arch));
    let machine = match machine {
        "x86_64" | "x86-64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{system}/{machine}")
}

/// Now in local time with its offset, to the nanosecond.
fn timestamp() -> String {
    let now = jiff::Zoned::now();
    now.strftime("%Y-%m-%dT%H:%M:%S%.9f%:z").to_string()
}
