//! The Python container of a run (decision 19): the Python packages without a stand-in that
//! workflow code imports run in one container, which the local container engine starts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::Cell;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use executor::process::{ContainerProcess, ContainerSpec, Mount, image_exists, prepare_image};
use python_evaluator::packages::{PythonProcess, PythonStart, PythonStarter};

/// The image that the Python images are prepared from when the user gives none: micromamba,
/// which installs Python and the packages from conda-forge and bioconda.
pub const DEFAULT_IMAGE: &str = "mambaorg/micromamba:2.3.3";

/// How the Python container of a run starts.
#[derive(Clone, Debug)]
pub struct PythonContainers {
    /// The container engine command, `docker` or `podman`.
    pub engine: String,
    /// An image with `python3` and the packages; `None` installs them into the default image.
    pub image: Option<String>,
    /// Where the conda packages that the default image downloads are kept between runs.
    pub package_cache: Option<PathBuf>,
    /// Passes what the process writes to standard error, like what packages print, on to the
    /// standard error of iunctus.
    pub show_output: bool,
}

/// Starts the Python container of a run, which sees the folders of the workflow.
pub struct ContainerStarter {
    pub settings: PythonContainers,
    /// The folders of the workflow, which the container mounts read-only at the same paths.
    pub folders: Vec<PathBuf>,
    started: Cell<u32>,
}

impl ContainerStarter {
    pub fn new(settings: PythonContainers, folders: Vec<PathBuf>) -> ContainerStarter {
        ContainerStarter {
            settings,
            folders: outermost(folders),
            started: Cell::new(0),
        }
    }
}

/// The folders without the ones inside another, which the container sees already.
fn outermost(mut folders: Vec<PathBuf>) -> Vec<PathBuf> {
    folders.sort();
    folders.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if !kept.iter().any(|outer| folder.starts_with(outer)) {
            kept.push(folder);
        }
    }
    kept
}

/// The script that prepares the image of a set of conda packages. It asks the channels for
/// each package first, since micromamba aborts when a solve fails, then makes the environment
/// of Python and the packages that the channels have, and lists both kinds in it. Python
/// compiles the modules itself: micromamba 2.3 aborts in the activation that it runs for that.
const PREPARE: &str = r#"set -u
: > /tmp/conda-missing
available=()
for package in "$@"; do
  if micromamba search --quiet --json --channel conda-forge --channel bioconda "$package" 2>/dev/null | grep -qF "\"name\": \"$package\""; then
    available+=("$package")
  else
    echo "$package" >> /tmp/conda-missing
  fi
done
micromamba create --yes --quiet --no-pyc --prefix /tmp/env --channel conda-forge --channel bioconda python=3.12 "${available[@]}" >&2 || exit 1
/tmp/env/bin/python -m compileall -q /tmp/env/lib > /dev/null 2>&1
printf '%s\n' "${available[@]}" > /tmp/env/conda-installed
mv /tmp/conda-missing /tmp/env/conda-missing
"#;

/// The file that makes the preparations of images and the installs of packages wait for each
/// other, since they share the package cache.
const LOCK: &str = ".iunctus-lock";

/// The FNV-1a hash of texts, which stays the same between versions of iunctus.
fn fingerprint(texts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for text in texts {
        for byte in text.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// The user and group that own a path, like `1000:1000`.
fn owner(path: &Path) -> Option<String> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| format!("{}:{}", metadata.uid(), metadata.gid()))
}

impl ContainerStarter {
    /// The package cache, made when it does not exist yet.
    fn cache(&self) -> Result<Option<PathBuf>, String> {
        let Some(cache) = &self.settings.package_cache else {
            return Ok(None);
        };
        std::fs::create_dir_all(cache)
            .map_err(|error| format!("cannot make the folder {}: {error}", cache.display()))?;
        Ok(Some(cache.clone()))
    }

    /// The environment of the default image, which installs into the package cache.
    fn conda_environment(cache: Option<&Path>) -> Vec<(String, String)> {
        let mut environment = vec![
            ("HOME".to_owned(), "/tmp".to_owned()),
            ("MAMBA_ROOT_PREFIX".to_owned(), "/tmp/mamba".to_owned()),
        ];
        if let Some(cache) = cache {
            environment.push((
                "CONDA_PKGS_DIRS".to_owned(),
                cache.to_string_lossy().into_owned(),
            ));
        }
        environment
    }

    /// The image with Python and a set of conda packages. It is prepared once, one
    /// preparation at a time on the machine, and kept for the next runs (decision 12).
    fn prepared_image(
        &self,
        conda: &[String],
        cache: Option<&Path>,
        user: Option<String>,
    ) -> Result<String, String> {
        let mut packages = conda.to_vec();
        packages.sort();
        packages.dedup();
        let key = fingerprint(&[DEFAULT_IMAGE, PREPARE, &packages.join(" ")]);
        let image = format!("iunctus-python:{key:016x}");
        let engine = &self.settings.engine;
        if image_exists(engine, &image) {
            return Ok(image);
        }
        let folder = cache.map_or_else(std::env::temp_dir, Path::to_path_buf);
        let lock = std::fs::File::options()
            .create(true)
            .append(true)
            .open(folder.join(LOCK))
            .and_then(|file| file.lock().map(|()| file))
            .map_err(|error| format!("cannot lock {}: {error}", folder.join(LOCK).display()))?;
        if !image_exists(engine, &image) {
            if self.settings.show_output {
                eprintln!(
                    "iunctus: preparing the Python image {image} with {}",
                    if packages.is_empty() {
                        "Python only".to_owned()
                    } else {
                        packages.join(", ")
                    }
                );
            }
            let mut command = vec!["-c".to_owned(), PREPARE.to_owned(), "iunctus".to_owned()];
            command.extend(packages.iter().cloned());
            let spec = ContainerSpec {
                engine: engine.clone(),
                name: format!("iunctus-python-prepare-{}-{key:016x}", std::process::id()),
                image: DEFAULT_IMAGE.to_owned(),
                user,
                entrypoint: Some("/bin/bash".to_owned()),
                command,
                mounts: cache
                    .map(|cache| Mount {
                        source: cache.to_path_buf(),
                        target: cache.to_string_lossy().into_owned(),
                        read_only: false,
                    })
                    .into_iter()
                    .collect(),
                workdir: None,
                environment: Self::conda_environment(cache),
                show_stderr: false,
            };
            prepare_image(&spec, &image).map_err(|error| error.to_string())?;
        }
        drop(lock);
        Ok(image)
    }
}

impl PythonStarter for ContainerStarter {
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String> {
        let number = self.started.get() + 1;
        self.started.set(number);
        let mut mounts: Vec<Mount> = self
            .folders
            .iter()
            .map(|folder| Mount {
                source: folder.clone(),
                target: folder.to_string_lossy().into_owned(),
                read_only: true,
            })
            .collect();
        let script = vec!["-u".to_owned(), "-c".to_owned(), start.script.to_owned()];
        let (image, entrypoint, command, environment, user) = match &self.settings.image {
            Some(image) => {
                let mut command = vec!["python3".to_owned()];
                command.extend(script);
                let environment = vec![("HOME".to_owned(), "/tmp".to_owned())];
                (image.clone(), None, command, environment, owner(&start.cwd))
            }
            None => {
                let cache = self.cache()?;
                let user = cache.as_deref().map_or_else(|| owner(&start.cwd), owner);
                let image = self.prepared_image(&start.conda, cache.as_deref(), user.clone())?;
                if let Some(cache) = &cache {
                    // Packages that later imports need are installed into the running container.
                    mounts.push(Mount {
                        source: cache.clone(),
                        target: cache.to_string_lossy().into_owned(),
                        read_only: false,
                    });
                }
                let environment = Self::conda_environment(cache.as_deref());
                let entrypoint = Some("/tmp/env/bin/python".to_owned());
                (image, entrypoint, script, environment, user)
            }
        };
        let spec = ContainerSpec {
            engine: self.settings.engine.clone(),
            name: format!("iunctus-python-{}-{number}", std::process::id()),
            image,
            user,
            entrypoint,
            command,
            mounts,
            workdir: Some(start.cwd.to_string_lossy().into_owned()),
            environment,
            show_stderr: self.settings.show_output,
        };
        let process = ContainerProcess::start(&spec).map_err(|error| error.to_string())?;
        Ok(Box::new(ContainerPython(process)))
    }
}

/// The Python process in its container.
struct ContainerPython(ContainerProcess);

impl PythonProcess for ContainerPython {
    fn exchange(&mut self, line: &str) -> Result<String, String> {
        self.0.exchange(line).map_err(|error| error.to_string())
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::outermost;

    #[test]
    fn folders_inside_others_are_not_mounted_again() {
        let folders = outermost(vec![
            PathBuf::from("/w/project/.test"),
            PathBuf::from("/w/project"),
            PathBuf::from("/w/other"),
            PathBuf::from("/w/project"),
        ]);
        assert_eq!(
            folders,
            [PathBuf::from("/w/other"), PathBuf::from("/w/project")]
        );
    }
}
