//! The Python container of a run (decision 19): the Python packages without a stand-in that
//! workflow code imports run in one container, which the local container engine starts. uv
//! installs them from PyPI into an environment that is kept for the next runs with the same
//! packages.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::Cell;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use executor::process::{ContainerProcess, ContainerSpec, Mount, run_to_end};
use python_evaluator::packages::{PythonProcess, PythonStart, PythonStarter};

/// The image of the Python container when the user gives none: a slim Python with uv.
pub const DEFAULT_IMAGE: &str = "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim";

/// How the Python container of a run starts.
#[derive(Clone, Debug)]
pub struct PythonContainers {
    /// The container engine command, `docker` or `podman`.
    pub engine: String,
    /// An image with `python3` and the packages; `None` installs them into the default image.
    pub image: Option<String>,
    /// Where uv keeps what it downloads (`uv`) and where the environments of the default
    /// image are kept by their packages (`environments`).
    pub cache: PathBuf,
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

/// The files where a workflow names the packages it runs with.
const ENVIRONMENT_FILES: &[&str] = &["environment.yaml", "environment.yml", "requirements.txt"];

/// The version pins of the workflow's own environment files in its folders, like
/// `yatiml==0.7`; files that do not list snakemake are the environments of rules instead.
pub fn workflow_pins(folders: &[PathBuf]) -> Vec<String> {
    let mut pins = Vec::new();
    let mut folders = folders.to_vec();
    folders.sort();
    folders.dedup();
    for folder in &folders {
        for name in ENVIRONMENT_FILES {
            if let Ok(text) = std::fs::read_to_string(folder.join(name)) {
                pins.extend(python_evaluator::packages::pins::requirements(name, &text));
            }
        }
    }
    pins
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

/// The script that makes the environment of a set of packages in the folder of its first
/// argument. It installs the packages together, and when that fails, one by one, keeping the
/// error of each package that uv cannot install. The finished environment appears at once.
const PREPARE: &str = r#"set -u
environment="$1"
shift
rm -rf "$environment.part"
uv venv --quiet --relocatable --python /usr/local/bin/python3 "$environment.part" || exit 1
mkdir -p "$environment.part/iunctus-failed"
install() {
  uv pip install --quiet --python "$environment.part/bin/python" "$@"
}
if [ "$#" -gt 0 ] && ! install "$@" 2> /dev/null; then
  for package in "$@"; do
    install "$package" 2> "$environment.part/iunctus-failed/$package" && rm "$environment.part/iunctus-failed/$package"
  done
fi
rm -rf "$environment"
mv "$environment.part" "$environment"
"#;

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
    fn uv_cache(&self) -> PathBuf {
        self.settings.cache.join("uv")
    }

    /// The environment of what every container runs with: uv's cache.
    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".to_owned(), "/tmp".to_owned()),
            (
                "UV_CACHE_DIR".to_owned(),
                self.uv_cache().to_string_lossy().into_owned(),
            ),
        ]
    }

    /// The environment of Python and a set of packages. It is made once, and kept for the
    /// next runs; a run waits while another one makes the same environment.
    fn prepared_environment(
        &self,
        packages: &[String],
        user: Option<String>,
    ) -> Result<PathBuf, String> {
        let mut packages = packages.to_vec();
        packages.sort();
        packages.dedup();
        let key = fingerprint(&[DEFAULT_IMAGE, PREPARE, &packages.join(" ")]);
        let folder = self.settings.cache.join("environments");
        let environment = folder.join(format!("{key:016x}"));
        if environment.is_dir() {
            return Ok(environment);
        }
        let lock = folder.join(format!("{key:016x}.lock"));
        let lock = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&lock)
            .and_then(|file| file.lock().map(|()| file))
            .map_err(|error| format!("cannot lock {}: {error}", lock.display()))?;
        if !environment.is_dir() {
            if self.settings.show_output {
                eprintln!(
                    "iunctus: installing {} for the Python container",
                    if packages.is_empty() {
                        "Python".to_owned()
                    } else {
                        packages.join(", ")
                    }
                );
            }
            let mut command = vec![
                "-c".to_owned(),
                PREPARE.to_owned(),
                "iunctus".to_owned(),
                environment.to_string_lossy().into_owned(),
            ];
            command.extend(packages.iter().cloned());
            let cache = &self.settings.cache;
            let spec = ContainerSpec {
                engine: self.settings.engine.clone(),
                name: format!("iunctus-python-prepare-{}-{key:016x}", std::process::id()),
                image: DEFAULT_IMAGE.to_owned(),
                user,
                entrypoint: Some("/bin/bash".to_owned()),
                command,
                mounts: vec![Mount {
                    source: cache.clone(),
                    target: cache.to_string_lossy().into_owned(),
                    read_only: false,
                }],
                workdir: None,
                environment: self.environment(),
                show_stderr: false,
            };
            run_to_end(&spec).map_err(|error| error.to_string())?;
        }
        drop(lock);
        Ok(environment)
    }
}

impl PythonStarter for ContainerStarter {
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String> {
        let number = self.started.get() + 1;
        self.started.set(number);
        let cache = &self.settings.cache;
        std::fs::create_dir_all(cache.join("environments"))
            .and_then(|()| std::fs::create_dir_all(self.uv_cache()))
            .map_err(|error| format!("cannot make the folder {}: {error}", cache.display()))?;
        let user = owner(cache);
        let mut mounts: Vec<Mount> = self
            .folders
            .iter()
            .map(|folder| Mount {
                source: folder.clone(),
                target: folder.to_string_lossy().into_owned(),
                read_only: true,
            })
            .collect();
        // uv's cache, for the packages that later imports install, and the environments.
        mounts.push(Mount {
            source: cache.clone(),
            target: cache.to_string_lossy().into_owned(),
            read_only: false,
        });
        let script = vec!["-u".to_owned(), "-c".to_owned(), start.script.to_owned()];
        let (image, entrypoint, command) = match &self.settings.image {
            Some(image) => {
                let mut command = vec!["python3".to_owned()];
                command.extend(script);
                (image.clone(), None, command)
            }
            None => {
                let environment = self.prepared_environment(&start.packages, user.clone())?;
                let python = environment
                    .join("bin/python")
                    .to_string_lossy()
                    .into_owned();
                (DEFAULT_IMAGE.to_owned(), Some(python), script)
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
            environment: self.environment(),
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
