//! Builds completed Conda images through the Docker-compatible classic build API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use bollard::query_parameters::{BuildImageOptions, BuilderVersion};
use conda_environment::{BuiltImage, CompletedImage, ImagePreparation};
use futures_util::StreamExt;

use crate::{CancellationToken, ContainerEngine};

const PREPARATION: &str = "org.iunctus.conda.preparation";
const BASE: &str = "org.iunctus.conda.base";
const RECIPE: &str = "org.iunctus.conda.recipe";

/// Finite context, progress, wall-clock and classic-builder resource allowances.
#[derive(Clone, Copy, Debug)]
pub struct BuildLimits {
    pub context_bytes: usize,
    pub progress_bytes: usize,
    pub timeout: Duration,
    /// Classic Docker build uses signed 32-bit byte allowances.
    pub memory_bytes: i32,
    /// CPU microseconds allowed in each 100,000 microsecond period.
    pub cpu_quota: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum CondaImageError {
    #[error("Conda image preparation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Conda image engine request failed: {0}")]
    Engine(#[from] bollard::errors::Error),
    #[error("Conda image operation was cancelled")]
    Cancelled,
    #[error("Conda image operation exceeded its {0} limit")]
    Limit(&'static str),
}

/// Builds with the classic builder only. Engines requiring BuildKit are unsupported.
/// Cancelling or timing out closes the request and returns no receipt; daemon-side build
/// termination is engine-dependent and is not confirmed by this API.
pub async fn build(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    package_cache: &Path,
    limits: BuildLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    if limits.context_bytes == 0
        || limits.progress_bytes == 0
        || limits.memory_bytes <= 0
        || limits.cpu_quota <= 0
        || limits.timeout.is_zero()
    {
        return Err(invalid("build limits must be positive"));
    }
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .ok_or_else(|| invalid("build timeout is too large"))?;
    let operation = cancel.child_token();
    let _guard = operation.clone().drop_guard();
    let work = async {
        let prepared = preparation.clone();
        let cache = package_cache.to_owned();
        let cancellation = operation.clone();
        let context = tokio::task::spawn_blocking(move || {
            let mut out = Context {
                bytes: Vec::new(),
                limit: limits.context_bytes,
                deadline,
                cancellation,
            };
            prepared.write_context(&cache, &mut out)?;
            Ok::<_, io::Error>(out.bytes)
        })
        .await
        .map_err(|error| invalid(&error.to_string()))??;
        let options = BuildImageOptions {
            labels: Some(HashMap::from([
                (PREPARATION.into(), preparation.digest().into()),
                (BASE.into(), preparation.base().into()),
                (RECIPE.into(), preparation.recipe_digest()),
            ])),
            platform: preparation.platform().into(),
            version: BuilderVersion::BuilderV1,
            networkmode: Some("none".into()),
            memory: Some(limits.memory_bytes),
            memswap: Some(limits.memory_bytes),
            cpuperiod: Some(100_000),
            cpuquota: Some(limits.cpu_quota),
            forcerm: true,
            ..BuildImageOptions::default()
        };
        let mut progress =
            engine
                .client()
                .build_image(options, None, Some(bollard::body_full(context.into())));
        let mut used = 0usize;
        let mut image_id = None;
        while let Some(step) = progress.next().await {
            let step = step?;
            let size = format!("{step:?}").len();
            used = used
                .checked_add(size)
                .filter(|used| *used <= limits.progress_bytes)
                .ok_or(CondaImageError::Limit("build progress"))?;
            if let Some(id) = step.aux.and_then(|aux| aux.id) {
                image_id = Some(id);
            } else if let Some(stream) = step.stream {
                for line in stream.lines() {
                    if let Some(id) = line.strip_prefix("Successfully built ") {
                        let id = id.trim();
                        if id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                            image_id = Some(format!("sha256:{id}"));
                        }
                    }
                }
            }
        }
        let id = image_id.ok_or_else(|| invalid("builder returned no full immutable image ID"))?;
        inspect(engine, preparation, &id).await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CondaImageError::Cancelled),
        result = tokio::time::timeout(limits.timeout, work) => {
            result.map_err(|_| CondaImageError::Limit("wall-clock"))?
        }
    }
}

async fn inspect(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    id: &str,
) -> Result<CompletedImage, CondaImageError> {
    if !id.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        return Err(invalid("builder returned an invalid immutable image ID"));
    }
    let image = engine.client().inspect_image(id).await?;
    if image.id.as_deref() != Some(id) {
        return Err(invalid(
            "inspected image ID differs from the completed build",
        ));
    }
    let mut platform = format!(
        "{}/{}",
        image.os.unwrap_or_default(),
        image.architecture.unwrap_or_default()
    );
    if let Some(variant) = image.variant.filter(|variant| !variant.is_empty()) {
        platform.push('/');
        platform.push_str(&variant);
    }
    let labels = image
        .config
        .and_then(|config| config.labels)
        .unwrap_or_default();
    Ok(preparation.completed(BuiltImage {
        preparation: labels.get(PREPARATION).cloned().unwrap_or_default(),
        base: labels.get(BASE).cloned().unwrap_or_default(),
        recipe_digest: labels.get(RECIPE).cloned().unwrap_or_default(),
        platform,
        image_id: id.into(),
        manifest_digest: None,
        archive_digest: None,
        reference: None,
    })?)
}

fn invalid(message: &str) -> CondaImageError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

struct Context {
    bytes: Vec<u8>,
    limit: usize,
    deadline: Instant,
    cancellation: CancellationToken,
}

impl Write for Context {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "context cancelled or timed out",
            ));
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("build context exceeds its byte limit"));
        }
        self.bytes
            .try_reserve_exact(bytes.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
