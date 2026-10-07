//! `iunctus lock-environment <file>`: offline locks or configured cached HTTPS acquisition
//! for an explicit container target. See docs/design/conda.md.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use conda_environment::{Environment, ImagePreparation, Lock, LockError, VirtualPackage};
use config_file::{ConfigError, ConfigFile};

use crate::arguments::Arguments;
use crate::failure::{Failure, failure};

/// The channel of an environment file that names none, as Miniforge has it.
const DEFAULT_CHANNEL: &str = "conda-forge";

/// Preparation inputs for one exact target base and platform.
#[derive(Clone, Debug)]
pub struct Config {
    pub base: String,
    pub platform: String,
    pub capabilities: Vec<VirtualPackage>,
    pub repodata: PathBuf,
    pub packages: PathBuf,
    pub offline: bool,
    pub acquisition: conda_environment::Acquisition,
}

impl Config {
    pub fn read(config: &ConfigFile) -> Result<Option<Self>, ConfigError> {
        let Some(table) = config.section("conda")? else {
            return Ok(None);
        };
        table.only(&[
            "base",
            "platform",
            "installer",
            "offline",
            "capabilities",
            "repodata",
            "packages",
        ])?;
        let offline = table.flag("offline")?.unwrap_or(true);
        if table.text("installer")?.as_deref() != Some("micromamba") {
            return Err(table.invalid(
                "installer",
                "must be micromamba, installed inside the pinned base",
            ));
        }
        let text = |key| {
            table
                .text(key)?
                .ok_or_else(|| table.missing(key, "an explicit value"))
        };
        let path = |key| -> Result<PathBuf, ConfigError> {
            let path = PathBuf::from(text(key)?);
            if !path.is_absolute() {
                return Err(table.invalid(key, "must be an absolute path"));
            }
            Ok(path)
        };
        let base = text("base")?;
        let platform = text("platform")?;
        let metadata = bounded_bytes(&path("capabilities")?, 64 * 1024)
            .map_err(|error| table.invalid("capabilities", error.to_string()))?;
        let metadata: serde_json::Value = serde_json::from_slice(&metadata)
            .map_err(|error| table.invalid("capabilities", error.to_string()))?;
        let object = metadata
            .as_object()
            .ok_or_else(|| table.invalid("capabilities", "must contain a JSON object"))?;
        if object
            .keys()
            .any(|key| !["base", "platform", "virtual_packages"].contains(&key.as_str()))
            || metadata["base"].as_str() != Some(base.as_str())
            || metadata["platform"].as_str() != Some(platform.as_str())
        {
            return Err(table.invalid(
                "capabilities",
                "must bind exactly this base and platform, with no unknown fields",
            ));
        }
        let mut capabilities: Vec<VirtualPackage> =
            serde_json::from_value(metadata["virtual_packages"].clone())
                .map_err(|error| table.invalid("capabilities", error.to_string()))?;
        if capabilities.len() > 64 {
            return Err(table.invalid("capabilities", "exceeds 64 virtual packages"));
        }
        if metadata["virtual_packages"]
            .as_array()
            .is_none_or(|packages| {
                packages.iter().any(|package| {
                    package.as_object().is_none_or(|object| {
                        object
                            .keys()
                            .any(|key| !["name", "version", "build"].contains(&key.as_str()))
                    })
                })
            })
        {
            return Err(table.invalid("capabilities", "has unknown virtual package fields"));
        }
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        for (index, package) in capabilities.iter().enumerate() {
            if !package.name.starts_with("__")
                || !conda_environment::MatchSpec::parse(&package.name)
                    .is_ok_and(|spec| spec.name == package.name)
                || conda_environment::Version::parse(&package.version).is_none()
                || (index > 0 && capabilities[index - 1].name == package.name)
            {
                return Err(
                    table.invalid("capabilities", "has invalid or duplicate virtual packages")
                );
            }
        }
        let repodata = path("repodata")?;
        let acquisition = conda_environment::Acquisition::new(&repodata, offline)
            .map_err(|error| table.invalid("repodata", error.to_string()))?;
        let configured = Self {
            base,
            platform,
            capabilities,
            repodata,
            packages: path("packages")?,
            offline,
            acquisition,
        };
        let lock = Lock {
            format: conda_environment::FORMAT,
            platform: configured
                .conda_platform()
                .map_err(|error| table.invalid("platform", error.to_string()))?
                .into(),
            channels: vec![],
            specs: vec![],
            pip: vec![],
            pypi: None,
            packages: vec![],
            virtual_packages: Some(configured.capabilities.clone()),
        };
        ImagePreparation::new(
            &lock,
            &configured.base,
            &configured.platform,
            &configured.capabilities,
        )
        .map_err(|error| table.invalid("base", error.to_string()))?;
        Ok(Some(configured))
    }

    fn conda_platform(&self) -> io::Result<&'static str> {
        match self.platform.as_str() {
            "linux/amd64" => Ok("linux-64"),
            "linux/arm64" | "linux/arm64/v8" => Ok("linux-aarch64"),
            "linux/ppc64le" => Ok("linux-ppc64le"),
            "linux/s390x" => Ok("linux-s390x"),
            "linux/386" => Ok("linux-32"),
            _ => Err(io::Error::other("unsupported Conda OCI platform")),
        }
    }

    /// Resolves current source bytes; paths and historical image mappings are never cache keys.
    pub fn preparation(&self, source: &str) -> io::Result<ImagePreparation> {
        let environment = Self::environment(source)?;
        let channels = conda_environment::channels(&environment.channels);
        let snapshot = tempfile::tempdir()?;
        let mut remaining = 256 * 1024 * 1024;
        let platform = self.conda_platform()?;
        for channel in channels {
            for subdir in [platform, "noarch"] {
                let source = conda_environment::repodata_path(&self.repodata, &channel, subdir);
                let bytes = bounded_bytes(&source, remaining)?;
                remaining -= bytes.len() as u64;
                let target = conda_environment::repodata_path(snapshot.path(), &channel, subdir);
                std::fs::create_dir_all(target.parent().expect("repodata parent"))?;
                std::fs::write(target, bytes)?;
            }
        }
        let lock = conda_environment::lock_with_cache(
            &environment,
            snapshot.path(),
            platform,
            &self.capabilities,
            &self.repodata,
        )
        .map_err(io::Error::other)?;
        ImagePreparation::new(&lock, &self.base, &self.platform, &self.capabilities)
    }
    pub async fn acquire_preparation(&self, source: &str) -> io::Result<ImagePreparation> {
        let environment = Self::environment(source)?;
        let lock = self
            .acquisition
            .lock(
                &environment,
                self.conda_platform()?,
                Some(&self.capabilities),
            )
            .await
            .map_err(io::Error::other)?;
        ImagePreparation::new(&lock, &self.base, &self.platform, &self.capabilities)
    }

    fn environment(source: &str) -> io::Result<Environment> {
        if source.contains("://")
            || !matches!(
                Path::new(source).extension().and_then(|s| s.to_str()),
                Some("yaml" | "yml")
            )
        {
            return Err(io::Error::other(
                "Conda preparation supports local YAML files only; remote files and named host environments are unsupported",
            ));
        }
        let bytes = bounded_bytes(Path::new(source), 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        let mut environment = Environment::parse(text).map_err(io::Error::other)?;
        conda_environment::validate_pip(&environment.pip).map_err(io::Error::other)?;
        if environment.channels.is_empty() {
            environment.channels.push(DEFAULT_CHANNEL.into());
        }
        let channels = conda_environment::channels(&environment.channels);
        if channels.len() > 16
            || channels
                .iter()
                .any(|channel| !conda_environment::valid_channel(channel))
        {
            return Err(io::Error::other(
                "unsupported Conda channel name or more than 16 channels",
            ));
        }
        Ok(environment)
    }
}

fn bounded_bytes(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Conda source must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(format!(
            "{} exceeds its Conda source byte limit",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Writes the lock of an environment file: as JSON to standard output, or to `-output`, where
/// a name that ends in `.txt` gets the explicit list that conda and micromamba install from.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(file) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name the environment file, like `iunctus lock-environment envs/qc.yaml`",
        ));
    };
    let text = std::fs::read_to_string(file)
        .map_err(|error| failure(format!("iunctus: cannot read {}: {error}", file.display())))?;
    let mut environment = Environment::parse(&text)
        .map_err(|error| failure(format!("iunctus: {}: {error}", file.display())))?;
    if environment.channels.is_empty() {
        environment.channels.push(DEFAULT_CHANNEL.into());
    }
    let configuration = ConfigFile::load(arguments.config_file.as_deref())
        .map_err(|error| Failure::setup(error.to_string()))?;
    let configured =
        Config::read(&configuration).map_err(|error| Failure::setup(error.to_string()))?;
    let platform = arguments.platform.clone().unwrap_or_else(|| {
        configured
            .as_ref()
            .and_then(|config| config.conda_platform().ok())
            .map(str::to_owned)
            .unwrap_or_else(conda_environment::platform)
    });
    let folder = repodata_folder();
    let lock = if let Some(config) = configured {
        if config
            .conda_platform()
            .map_err(|error| failure(error.to_string()))?
            != platform
        {
            return Err(failure(
                "the lock platform must match the configured Conda target",
            ));
        }
        let source = file
            .to_str()
            .ok_or_else(|| failure("Conda source path must be UTF-8"))?;
        let preparation = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        if config.offline {
                            match config.preparation(source) {
                                Ok(preparation) => return Ok(preparation),
                                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                                    return Err(error);
                                }
                                Err(_) => {}
                            }
                        }
                        config.acquire_preparation(source).await
                    })
                })
                .join()
        })
        .map_err(|_| failure("Conda lock worker failed"))?
        .map_err(|error| {
            if error
                .get_ref()
                .and_then(|error| error.downcast_ref::<LockError>())
                .is_some_and(|error| {
                    matches!(
                        error,
                        LockError::Pip(conda_environment::PipError::Unsupported(_))
                    )
                })
                || error
                    .get_ref()
                    .and_then(|error| error.downcast_ref::<conda_environment::PipError>())
                    .is_some_and(|error| {
                        matches!(error, conda_environment::PipError::Unsupported(_))
                    })
            {
                return Failure {
                    message: error.to_string(),
                    code: crate::failure::UNSUPPORTED,
                    after: vec![],
                };
            }
            failure(error.to_string())
        })?;
        preparation.lock().clone()
    } else {
        conda_environment::lock(&environment, &folder, &platform)
            .map_err(|error| locked(file, error))?
    };
    let Some(output) = &arguments.output else {
        println!("{}", json(&lock));
        return Ok(());
    };
    let explicit = output
        .extension()
        .is_some_and(|extension| extension == "txt");
    if explicit && !lock.pip.is_empty() {
        return Err(Failure {
            message:
                "pip locks require JSON output; a Conda explicit list cannot preserve pip pins"
                    .into(),
            code: crate::failure::UNSUPPORTED,
            after: vec![],
        });
    }
    let written = match explicit {
        true => lock.explicit(),
        false => format!("{}\n", json(&lock)),
    };
    std::fs::write(output, written).map_err(|error| {
        failure(format!(
            "iunctus: cannot write {}: {error}",
            output.display()
        ))
    })?;
    println!(
        "iunctus: locked {} packages for {platform} in {}; the environment is {}",
        lock.packages.len(),
        output.display(),
        lock.digest()
    );
    Ok(())
}

/// `<cache>/conda`, where the repodata of each channel and platform is kept.
pub fn repodata_folder() -> PathBuf {
    crate::cache_directory().join("conda")
}

fn json(lock: &Lock) -> String {
    // A lock is texts and lists, which always serialize.
    serde_json::to_string_pretty(lock).unwrap_or_default()
}

/// Missing repodata is for the machine to fix; the rest is the environment's.
fn locked(file: &Path, error: LockError) -> Failure {
    let message = format!("iunctus: {}: {error}", file.display());
    match error {
        LockError::Pip(conda_environment::PipError::Unsupported(_)) => Failure {
            message,
            code: crate::failure::UNSUPPORTED,
            after: vec![],
        },
        LockError::MissingRepodata { .. }
        | LockError::Io { .. }
        | LockError::Repodata { .. }
        | LockError::Pip(conda_environment::PipError::Io(_))
        | LockError::Acquisition(_) => Failure::setup(message),
        _ => failure(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_fifo_sources() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("source.yaml");
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, rustix::fs::Mode::RUSR).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _folder = folder;
            sender
                .send(bounded_bytes(&path, 1024).unwrap_err().to_string())
                .unwrap();
        });
        let error = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("FIFO open must not block");
        assert!(error.contains("regular file"), "{error}");
    }

    #[test]
    fn preparation_refreshes_metadata() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let config = Config {
            base: format!("mamba@sha256:{}", "a".repeat(64)),
            platform: "linux/amd64".into(),
            capabilities: vec![],
            repodata: root.join("repodata"),
            packages: root.join("missing-packages"),
            offline: true,
            acquisition: conda_environment::Acquisition::new(&root.join("repodata"), true).unwrap(),
        };
        let channel = config.repodata.join("conda-forge");
        for subdir in ["linux-64", "noarch"] {
            std::fs::create_dir_all(channel.join(subdir)).unwrap();
        }
        std::fs::write(channel.join("noarch/repodata.json"), "{}").unwrap();
        let mut records = json!({"packages": {
            "foo-1-0.conda": {"name":"foo", "version":"1", "build":"0", "build_number":0, "subdir":"linux-64", "sha256":"b".repeat(64)},
            "foo-2-0.conda": {"name":"foo", "version":"2", "build":"0", "build_number":0, "subdir":"linux-64", "sha256":"c".repeat(64)}
        }});
        let repodata = channel.join("linux-64/repodata.json");
        std::fs::write(&repodata, records.to_string()).unwrap();
        let file = root.join("env.yaml");
        std::fs::write(&file, "dependencies: [foo=1]\n").unwrap();
        let first = config.preparation(file.to_str().unwrap()).unwrap();
        std::fs::write(&file, "dependencies: [foo=2]\n").unwrap();
        let second = config.preparation(file.to_str().unwrap()).unwrap();
        assert_ne!(first.digest(), second.digest());
        records["packages"]["foo-2-0.conda"]["sha256"] = json!("d".repeat(64));
        std::fs::write(repodata, records.to_string()).unwrap();
        assert_ne!(
            second.digest(),
            config.preparation(file.to_str().unwrap()).unwrap().digest()
        );
        for unsupported in ["host-environment", "https://example.org/env.yaml"] {
            assert!(
                config
                    .preparation(unsupported)
                    .unwrap_err()
                    .to_string()
                    .contains("local YAML")
            );
        }
        std::fs::write(&file, "dependencies:\n  - pip:\n    - '-e .'\n").unwrap();
        assert!(
            config
                .preparation(file.to_str().unwrap())
                .unwrap_err()
                .to_string()
                .contains("pip")
        );
        std::fs::write(&file, vec![b' '; 1024 * 1024 + 1]).unwrap();
        assert!(
            config
                .preparation(file.to_str().unwrap())
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
    }
}
