//! Cached Conda channel acquisition through one shared HTTPS client.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rattler_conda_types::package::BuildString;
use rattler_conda_types::{Channel, ChannelConfig, GenericVirtualPackage, Subdir};
use rattler_repodata_gateway::{ChannelRelationsMode, Gateway, fetch::CacheAction};
use sha2::{Digest as _, Sha256};
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
        crate::validate_pip(&environment.pip)?;
        let specs = crate::conda_specs(environment);
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
        let request = self.requests.clone().lock_owned().await;
        let query = self
            .gateway
            .query(
                sources,
                [platform, Subdir::NoArch],
                specs
                    .iter()
                    .filter(|spec| !spec.name.starts_with("__"))
                    .map(|spec| spec.native.clone()),
            )
            .recursive(true)
            .channel_notices(false)
            .channel_relations(ChannelRelationsMode::Disabled)
            .await
            .map_err(|error| LockError::Acquisition(if self.offline {
                format!("{error}; offline Conda metadata cache is incomplete at {}. Populate it with a network-enabled run using [conda].offline = false, or supply prepared channel repodata.json files in [conda].repodata before rerunning offline", self.cache.display())
            } else { error.to_string() }))?;
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
                build_string: BuildString::new_unchecked(package.build.clone()),
            })
            .collect();
        let records = crate::solve::resolve(
            query.iter().map(|bucket| bucket.iter().collect()).collect(),
            &specs,
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
        let mut lock = Lock {
            format: crate::FORMAT,
            platform: platform.to_string(),
            channels,
            specs: environment
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect(),
            pip: environment.pip.clone(),
            pypi: None,
            packages,
            virtual_packages,
        };
        if !lock.pip.is_empty() {
            let cache = self.cache.clone();
            let offline = self.offline;
            lock = tokio::task::spawn_blocking(move || {
                let _request = request;
                lock_pip_cached(&mut lock, &cache, offline)?;
                Ok::<_, LockError>(lock)
            })
            .await
            .map_err(|error| LockError::Acquisition(error.to_string()))??;
        }
        Ok(lock)
    }

    /// Reuses verified bytes or streams packages serially into the SHA-256 cache.
    /// Failed, corrupt or cancelled downloads leave the previous cache entry intact.
    pub async fn fetch_packages(&self, lock: &Lock, folder: &Path) -> io::Result<Vec<PathBuf>> {
        let _request = self.requests.lock().await;
        if self.offline {
            return crate::fetch_packages(lock, folder, |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Conda package is not cached and offline mode forbids downloading. Populate {} with a network-enabled run using [conda].offline = false, or copy verified package bytes under their SHA-256 filenames into [conda].packages before rerunning offline",
                        folder.display()
                    ),
                ))
            });
        }
        std::fs::create_dir_all(folder)?;
        let mut paths = Vec::new();
        let artifacts = crate::fetch::artifacts(lock)?;
        for (url, digest) in artifacts {
            let path = folder.join(&digest);
            if crate::fetch::verify(&path, &digest).is_err() {
                let mut response = self
                    .client
                    .get(url)
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

pub(crate) fn lock_pip_cached(
    lock: &mut Lock,
    folder: &Path,
    offline: bool,
) -> Result<(), LockError> {
    if lock.pip.is_empty() {
        return Ok(());
    }
    let client = if offline {
        None
    } else {
        Some(
            reqwest::blocking::Client::builder()
                .https_only(true)
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(300))
                .build()
                .map_err(|error| LockError::Acquisition(error.to_string()))?,
        )
    };
    let folder = folder.join("pypi");
    crate::lock_pip(lock, |url| {
        let key: String = Sha256::digest(url.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = folder.join(key);
        if (offline || !url.starts_with("https://pypi.org/simple/")) && path.is_file() {
            return limited(std::fs::File::open(path)?);
        }
        let client = client.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("pip metadata is not cached and offline mode forbids downloading: {url}"),
            )
        })?;
        let response = client
            .get(url)
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.pypi.simple.v1+json",
            )
            .send()
            .map_err(io::Error::other)?
            .error_for_status()
            .map_err(io::Error::other)?;
        let bytes = limited(response)?;
        std::fs::create_dir_all(&folder)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&folder)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(bytes)
    })?;
    Ok(())
}

fn limited(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take((64 << 20) + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 << 20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pip metadata or artifact exceeds 64 MiB locking limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[tokio::test]
    async fn fetches_local_packages() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/package.conda", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nbytes",
                    )
                    .unwrap();
            }
        });
        let cache = tempfile::tempdir().unwrap();
        let digest: String = Sha256::digest(b"bytes")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut lock = Lock {
            format: crate::FORMAT,
            platform: "linux-64".into(),
            channels: vec![],
            specs: vec![],
            pip: vec![],
            pypi: None,
            virtual_packages: Some(vec![]),
            packages: vec![crate::Package {
                name: "fixture".into(),
                version: "1".into(),
                build: "0".into(),
                channel: "fixture".into(),
                subdir: "noarch".into(),
                file: "fixture-1-0.conda".into(),
                url,
                sha256: Some(digest.clone()),
                md5: None,
            }],
        };
        let mut acquisition = Acquisition::new(cache.path(), false).unwrap();
        // Only this test client accepts loopback HTTP; production acquisition requires HTTPS.
        acquisition.client = reqwest::Client::builder().no_proxy().build().unwrap();
        let paths = acquisition
            .fetch_packages(&lock, cache.path())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"bytes");
        assert_eq!(
            acquisition
                .fetch_packages(&lock, cache.path())
                .await
                .unwrap(),
            paths
        );
        lock.packages[0].sha256 = Some("a".repeat(64));
        let error = acquisition
            .fetch_packages(&lock, cache.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("SHA-256 mismatch"));
        server.join().unwrap();
        assert!(!cache.path().join("a".repeat(64)).exists());
        assert_eq!(std::fs::read(cache.path().join(digest)).unwrap(), b"bytes");
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
    }
}
