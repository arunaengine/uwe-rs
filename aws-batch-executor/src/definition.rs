//! The container job definitions of the jobs: one per image and settings, found again by its
//! name and digest parameter, or registered once per run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::client::{BatchError, Client, LOG_GROUP};
use crate::settings::{Helper, Settings};

/// The prefix of the names of the job definitions that uwe registers, which keeps them apart
/// from those of Nextflow, whose command and settings differ.
pub const PREFIX: &str = "uwe-";

/// The parameter that holds the digest of the definition's settings.
pub const DIGEST: &str = "uwe-digest";

/// The volume of the folder of the helper on the host, for [`Helper::Path`].
const HELPER_VOLUME: &str = "uwe-helper";

/// How often a registration that AWS answers with 404 is tried again, as Nextflow does.
const REGISTER_TRIES: u32 = 6;

/// What a definition holds for one task besides the settings: its image, and on Fargate its
/// disk and CPU architecture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub image: String,
    /// The ephemeral storage of a Fargate task in GiB; `None` for Fargate's default of 50.
    pub disk_gib: Option<u64>,
    /// Whether a Fargate task runs on ARM64.
    pub arm: bool,
}

/// The definitions of a run, by digest: `<name>:<revision>`.
#[derive(Debug, Default)]
pub struct Definitions {
    known: tokio::sync::Mutex<HashMap<String, String>>,
}

/// The name of the definition of `image`: `uwe-` and the image with each run of other
/// characters than letters, digits, `-` and `_` as one `-`. A long name keeps its first 40
/// characters and the hash of the image.
pub fn name(image: &str) -> Result<String, String> {
    let mut clean = String::new();
    for character in image.chars() {
        match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => clean.push(character),
            _ if !clean.ends_with('-') => clean.push('-'),
            _ => {}
        }
    }
    let clean = clean.trim_matches('-');
    if clean.is_empty() {
        return Err(format!("invalid container image name: {image}"));
    }
    let name = format!("{PREFIX}{clean}");
    if name.len() <= 125 {
        return Ok(name);
    }
    let hash = hex(&Sha256::digest(image.as_bytes()));
    Ok(format!("{}-{}", &name[..40], &hash[..32]))
}

/// The body of `registerjobdefinition` for a task of `shape` in `region`, with the digest of all
/// else in its parameters.
pub fn body(shape: &Shape, settings: &Settings, region: &str) -> Result<Value, String> {
    let memory = if settings.fargate { "2048" } else { "1024" };
    let mut container = json!({
        "image": shape.image,
        "command": ["true"],
        "resourceRequirements": [
            {"type": "VCPU", "value": "1"},
            {"type": "MEMORY", "value": memory},
        ],
    });
    if let Some(role) = &settings.job_role {
        container["jobRoleArn"] = json!(role);
    }
    if let Some(role) = &settings.execution_role {
        container["executionRoleArn"] = json!(role);
    }
    if let Some(group) = &settings.logs_group {
        container["logConfiguration"] = json!({
            "logDriver": "awslogs",
            "options": {"awslogs-region": region, "awslogs-group": group},
        });
    }
    let (mut volumes, mut mounts) = (Vec::new(), Vec::new());
    for (number, volume) in settings.volumes.iter().enumerate() {
        let name = format!("vol-{}", number + 1);
        volumes.push(json!({"name": name, "host": {"sourcePath": volume.host}}));
        mounts.push(json!({
            "sourceVolume": name,
            "containerPath": volume.container,
            "readOnly": volume.read_only,
        }));
    }
    if let Some(Helper::Path(path)) = &settings.helper {
        if settings.fargate {
            return Err(
                "Fargate has no host folders for helper_path; use helper_url or let \
                        uwe upload the helper"
                    .into(),
            );
        }
        let folder = match path.rsplit_once('/') {
            Some(("", _)) | None => "/",
            Some((folder, _)) => folder,
        };
        volumes.push(json!({"name": HELPER_VOLUME, "host": {"sourcePath": folder}}));
        mounts.push(json!({
            "sourceVolume": HELPER_VOLUME,
            "containerPath": folder,
            "readOnly": true,
        }));
    }
    if !volumes.is_empty() {
        container["volumes"] = json!(volumes);
        container["mountPoints"] = json!(mounts);
    }
    let mut body = json!({
        "jobDefinitionName": name(&shape.image)?,
        "type": "container",
        "containerProperties": container,
    });
    if settings.fargate {
        let properties = &mut body["containerProperties"];
        properties["networkConfiguration"] = json!({"assignPublicIp": "ENABLED"});
        properties["ephemeralStorage"] = json!({"sizeInGiB": shape.disk_gib.unwrap_or(50)});
        if shape.arm {
            properties["runtimePlatform"] =
                json!({"cpuArchitecture": "ARM64", "operatingSystemFamily": "LINUX"});
        }
        body["platformCapabilities"] = json!(["FARGATE"]);
    } else if shape.arm {
        tracing::debug!(
            image = shape.image,
            "the queue decides the CPU architecture on EC2"
        );
    }
    let digest = hex(&Sha256::digest(body.to_string().as_bytes()));
    body["parameters"] = json!({DIGEST: digest});
    Ok(body)
}

impl Definitions {
    /// The `<name>:<revision>` of a definition with the settings of `body`: an active one that
    /// AWS has already, or else a new one.
    pub async fn resolve(&self, client: &Client, body: &Value) -> Result<String, BatchError> {
        let digest = body["parameters"][DIGEST].as_str().unwrap_or_default();
        let name = body["jobDefinitionName"].as_str().unwrap_or_default();
        // One registration at a time, so that tasks of the same image share one definition.
        let mut known = self.known.lock().await;
        if let Some(found) = known.get(digest) {
            return Ok(found.clone());
        }
        let found = match active(client, name, digest).await? {
            Some(found) => found,
            None => register(client, body).await?,
        };
        tracing::debug!(definition = found, "[AWS BATCH] job definition");
        known.insert(digest.to_owned(), found.clone());
        Ok(found)
    }
}

/// An active revision of `name` with `digest` in its parameters.
async fn active(client: &Client, name: &str, digest: &str) -> Result<Option<String>, BatchError> {
    let mut token: Option<String> = None;
    loop {
        let mut ask = json!({"jobDefinitionName": name, "status": "ACTIVE"});
        if let Some(token) = &token {
            ask["nextToken"] = json!(token);
        }
        let answer = client.batch("describejobdefinitions", &ask).await?;
        let definitions = answer["jobDefinitions"].as_array().cloned();
        for definition in definitions.unwrap_or_default() {
            if definition["status"] == "ACTIVE"
                && definition["parameters"][DIGEST] == digest
                && let Some(revision) = definition["revision"].as_i64()
            {
                return Ok(Some(format!("{name}:{revision}")));
            }
        }
        token = answer["nextToken"].as_str().map(str::to_owned);
        if token.is_none() {
            return Ok(None);
        }
    }
}

async fn register(client: &Client, body: &Value) -> Result<String, BatchError> {
    let mut tries = 0;
    loop {
        match client.batch("registerjobdefinition", body).await {
            Err(BatchError::Status { status: 404, .. }) if tries + 1 < REGISTER_TRIES => {
                tries += 1;
                let pause = Duration::from_millis(250 * 3u64.pow(tries));
                tokio::time::sleep(pause).await;
            }
            Err(error) => return Err(error),
            Ok(answer) => {
                let name = answer["jobDefinitionName"].as_str().unwrap_or_default();
                let revision = answer["revision"].as_i64().unwrap_or_default();
                return Ok(format!("{name}:{revision}"));
            }
        }
    }
}

/// The log group of the jobs: the configured one, else AWS's default.
pub fn log_group(settings: &Settings) -> &str {
    settings.logs_group.as_deref().unwrap_or(LOG_GROUP)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_definitions() {
        let image = "quay.io/biocontainers/samtools:1.21--h50ea8bc_0";
        let expected = "uwe-quay-io-biocontainers-samtools-1-21--h50ea8bc_0";
        assert_eq!(name(image).as_deref(), Ok(expected));
        assert_eq!(name("ubuntu:24.04").as_deref(), Ok("uwe-ubuntu-24-04"));
        let long = format!("registry.example.org/{}:1", "a".repeat(120));
        let named = name(&long).expect("a name");
        assert_eq!(named.len(), 40 + 1 + 32);
        assert!(named.starts_with("uwe-registry-example-org-aaa"), "{named}");
        assert!(name("::").is_err());
    }

    #[test]
    fn fargate_needs_downloads() {
        let settings = Settings {
            fargate: true,
            helper: Some(Helper::Path("/opt/uwe".into())),
            ..Settings::default()
        };
        let shape = Shape {
            image: "ubuntu:24.04".into(),
            ..Shape::default()
        };
        assert!(body(&shape, &settings, "eu-west-1").is_err());
        let settings = Settings {
            helper: None,
            ..settings
        };
        let shape = Shape {
            disk_gib: Some(80),
            arm: true,
            ..shape
        };
        let body = body(&shape, &settings, "eu-west-1").expect("a body");
        assert_eq!(body["platformCapabilities"], json!(["FARGATE"]));
        let properties = &body["containerProperties"];
        assert_eq!(properties["ephemeralStorage"]["sizeInGiB"], 80);
        assert_eq!(properties["runtimePlatform"]["cpuArchitecture"], "ARM64");
        assert_eq!(properties["resourceRequirements"][1]["value"], "2048");
    }
}
