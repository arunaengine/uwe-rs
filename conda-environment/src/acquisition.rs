//! Cached Conda channel acquisition through one shared HTTPS client.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rattler_conda_types::{Channel, ChannelConfig, GenericVirtualPackage, Subdir};
use rattler_repodata_gateway::{ChannelRelationsMode, Gateway, fetch::CacheAction};
use tokio::sync::Mutex;

use crate::{Environment, Lock, LockError, Package, Record, VirtualPackage, base, channels};

/// Clones share the HTTP connection pool, gateway and request serialization.
#[derive(Clone)]
pub struct Acquisition {
    client: reqwest::Client,
    gateway: Gateway,
    requests: Arc<Mutex<()>>,
    cache: PathBuf,
    offline: bool,
}

impl fmt::Debug for Acquisition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Acquisition")
            .field("offline", &self.offline)
            .finish_non_exhaustive()
    }
}

impl Acquisition {
    /// Offline queries use only cached gateway metadata, including stale metadata.
    /// Local operator-prepared JSON remains available through `lock_with_virtual`.
    pub fn new(cache: &Path, offline: bool) -> io::Result<Self> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(io::Error::other)?;
        let mut config = rattler_repodata_gateway::ChannelConfig::default();
        if offline {
            config.default.cache_action = CacheAction::ForceCacheOnly;
        }
        let gateway = Gateway::builder()
            .with_client(client.clone())
            .with_cache_dir(cache.join("gateway"))
            .with_channel_config(config)
            .with_max_concurrent_requests(1usize)
            .with_max_concurrent_io(1usize)
            .finish();
        Ok(Self {
            client,
            gateway,
            requests: Arc::new(Mutex::new(())),
            cache: cache.to_owned(),
            offline,
        })
    }

    /// Fetches only requested names and their dependency closure. The gateway supports
    /// sharded and zstd repodata with persistent caching. Target capabilities are supplied.
    pub async fn lock(
        &self,
        environment: &Environment,
        platform: &str,
        capabilities: Option<&[VirtualPackage]>,
    ) -> Result<Lock, LockError> {
        if !environment.pip.is_empty() {
            return Err(LockError::Pip);
        }
        let virtual_packages = crate::validate_capabilities(capabilities)?;
        let channels = channels(&environment.channels);
        let platform: Subdir = platform
            .parse()
            .map_err(|error| LockError::Acquisition(format!("invalid platform: {error}")))?;
        let config = ChannelConfig::default_with_root_dir(self.cache.clone());
        let sources = channels
            .iter()
            .map(|channel| {
                if !crate::valid_channel(channel) {
                    return Err(LockError::Acquisition("invalid Conda HTTPS channel".into()));
                }
                Channel::from_str(base(channel), &config)
                    .map_err(|error| LockError::Acquisition(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let _request = self.requests.lock().await;
        let query = self
            .gateway
            .query(
                sources,
                [platform, Subdir::NoArch],
                environment
                    .dependencies
                    .iter()
                    .filter(|spec| !spec.name.starts_with("__"))
                    .map(|spec| spec.native.clone()),
            )
            .recursive(true)
            .channel_notices(false)
            .channel_relations(ChannelRelationsMode::Disabled)
            .await
            .map_err(|error| LockError::Acquisition(error.to_string()))?;
        let capabilities = virtual_packages
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|package| GenericVirtualPackage {
                name: package
                    .name
                    .parse()
                    .expect("validated virtual package name"),
                version: package
                    .version
                    .parse()
                    .expect("validated virtual package version"),
                build_string: package.build.clone(),
            })
            .collect();
        let records = crate::solve::resolve(
            query.iter().map(|bucket| bucket.iter().collect()).collect(),
            &environment.dependencies,
            capabilities,
            &channels,
        )?;
        let packages = records
            .into_iter()
            .map(|native| {
                let channel = channels
                    .iter()
                    .find(|channel| {
                        native
                            .channel
                            .as_deref()
                            .is_some_and(|url| url.trim_end_matches('/') == base(channel))
                    })
                    .cloned()
                    .unwrap_or_else(|| native.channel.clone().unwrap_or_default());
                Record::from_native(native, channel)
                    .map(|record| Package::of(&record))
                    .map_err(|error| LockError::Acquisition(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Lock {
            format: crate::FORMAT,
            platform: platform.to_string(),
            channels,
            specs: environment
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect(),
            pip: vec![],
            pypi: None,
            packages,
            virtual_packages,
        })
    }

    /// Reuses verified bytes or streams packages serially into the SHA-256 cache.
    /// Failed, corrupt or cancelled downloads leave the previous cache entry intact.
    pub async fn fetch_packages(&self, lock: &Lock, folder: &Path) -> io::Result<Vec<PathBuf>> {
        let _request = self.requests.lock().await;
        if self.offline {
            return crate::fetch_packages(lock, folder, |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Conda package is not cached and offline mode forbids downloading",
                ))
            });
        }
        std::fs::create_dir_all(folder)?;
        let mut paths = Vec::new();
        for package in &lock.packages {
            let digest = crate::fetch::package_digest(package)?;
            let path = folder.join(&digest);
            if crate::fetch::verify(&path, &digest).is_err() {
                let mut response = self
                    .client
                    .get(&package.url)
                    .send()
                    .await
                    .map_err(io::Error::other)?
                    .error_for_status()
                    .map_err(io::Error::other)?;
                let mut temporary = tempfile::NamedTempFile::new_in(folder)?;
                let mut remaining = 4u64 << 30;
                while let Some(chunk) = response.chunk().await.map_err(io::Error::other)? {
                    remaining = remaining.checked_sub(chunk.len() as u64).ok_or_else(|| {
                        io::Error::other("Conda package exceeds 4 GiB download limit")
                    })?;
                    temporary.write_all(&chunk)?;
                }
                temporary.as_file().sync_all()?;
                crate::fetch::verify(temporary.path(), &digest)?;
                temporary.persist(&path).map_err(|error| error.error)?;
            }
            paths.push(path);
        }
        Ok(paths)
    }
}
