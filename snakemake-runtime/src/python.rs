//! Starts the run's Python container and installs external packages into reusable environments.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::TryLockError;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use executor::CancellationToken;
use executor::process::{ContainerProcess, ContainerSpec, Mount, ProcessLimits, run_with_limit};
use python_evaluator::packages::{PythonProcess, PythonStart, PythonStarter};

/// The image of the Python container when the user gives none: a slim Python with uv.
pub const DEFAULT_IMAGE: &str = "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim";

const HELPER_MEMORY: u64 = 1 << 30;
const PREPARE_MEMORY: u64 = 2 << 30;
const FRAME_LIMIT: usize = 16 << 20;
const REPLY_CAPACITY: usize = 1;
const HELPER_DEADLINE: Duration = Duration::from_secs(600);
const PREPARE_DEADLINE: Duration = Duration::from_secs(600);

/// How the Python container of a run starts.
#[derive(Clone, Debug)]
pub struct PythonContainers {
    /// The container engine as `UWE_ENGINE` names it: `docker`, `podman` or a socket;
    /// empty for Docker, else Podman.
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
    cancel: CancellationToken,
}

impl ContainerStarter {
    pub fn new(settings: PythonContainers, folders: Vec<PathBuf>) -> ContainerStarter {
        Self::with_cancel(settings, folders, CancellationToken::new())
    }

    pub fn with_cancel(
        settings: PythonContainers,
        folders: Vec<PathBuf>,
        cancel: CancellationToken,
    ) -> ContainerStarter {
        ContainerStarter {
            settings,
            folders: outermost(folders),
            cancel,
        }
    }
}

fn container_name(kind: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "uwe-python-{kind}-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
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
constraints="$2"
shift 2
rm -rf "$environment.part"
uv venv --quiet --relocatable --python /usr/local/bin/python3 "$environment.part" || exit 1
mkdir -p "$environment.part/uwe-failed"
printf '%s\n' "$constraints" > "$environment.part/uwe-constraints.txt"
install() {
  uv pip install --quiet --python "$environment.part/bin/python" --constraint "$environment.part/uwe-constraints.txt" "$@"
}
install packaging || exit 1
if [ "$#" -gt 0 ] && ! install "$@" 2> /dev/null; then
  for package in "$@"; do
    install "$package" 2> "$environment.part/uwe-failed/$package" && rm "$environment.part/uwe-failed/$package"
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
    fn environment(&self, constraints: &[String]) -> Vec<(String, String)> {
        vec![
            ("HOME".to_owned(), "/tmp".to_owned()),
            (
                "UV_CACHE_DIR".to_owned(),
                self.uv_cache().to_string_lossy().into_owned(),
            ),
            (
                "UWE_PYTHON_CONSTRAINTS".to_owned(),
                serde_json::to_string(constraints).expect("string constraints serialize"),
            ),
        ]
    }

    /// The environment of Python and a set of packages. It is made once, and kept for the
    /// next runs; a run waits while another one makes the same environment.
    fn prepared_environment(
        &self,
        packages: &[String],
        constraints: &[String],
        user: Option<String>,
    ) -> Result<PathBuf, String> {
        let mut packages = packages.to_vec();
        packages.sort();
        packages.dedup();
        let mut constraints = constraints.to_vec();
        constraints.sort();
        constraints.dedup();
        let key = fingerprint(&[
            DEFAULT_IMAGE,
            PREPARE,
            &packages.join("\n"),
            &constraints.join("\n"),
        ]);
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
            .map_err(|error| format!("cannot lock {}: {error}", lock.display()))?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            if self.cancel.is_cancelled() {
                return Err("Python environment preparation cancelled".into());
            }
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err("Python environment lock timed out".into());
                }
                Err(TryLockError::Error(error)) => return Err(error.to_string()),
            }
        }
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
                "uwe".to_owned(),
                environment.to_string_lossy().into_owned(),
                constraints.join("\n"),
            ];
            command.extend(packages.iter().cloned());
            let cache = &self.settings.cache;
            let spec = ContainerSpec {
                engine: self.settings.engine.clone(),
                name: container_name("prepare"),
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
                environment: self.environment(&constraints),
                memory: Some(PREPARE_MEMORY),
                show_stderr: false,
            };
            run_with_limit(&spec, &self.cancel, PREPARE_DEADLINE)
                .map_err(|error| error.to_string())?;
        }
        drop(lock);
        Ok(environment)
    }
}

impl PythonStarter for ContainerStarter {
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String> {
        if self.cancel.is_cancelled() {
            return Err("Python helper start cancelled".into());
        }
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
                let environment =
                    self.prepared_environment(&start.packages, &start.constraints, user.clone())?;
                let python = environment
                    .join("bin/python")
                    .to_string_lossy()
                    .into_owned();
                (DEFAULT_IMAGE.to_owned(), Some(python), script)
            }
        };
        let spec = ContainerSpec {
            engine: self.settings.engine.clone(),
            name: container_name("helper"),
            image,
            user,
            entrypoint,
            command,
            mounts,
            workdir: Some(start.cwd.to_string_lossy().into_owned()),
            environment: self.environment(&start.constraints),
            memory: Some(HELPER_MEMORY),
            show_stderr: self.settings.show_output,
        };
        let process = ContainerProcess::start_bounded(
            &spec,
            ProcessLimits {
                max_frame_bytes: FRAME_LIMIT,
                reply_capacity: REPLY_CAPACITY,
            },
            &self.cancel,
        )
        .map_err(|error| error.to_string())?;
        Ok(Box::new(ContainerPython(process)))
    }
}

/// The Python process in its container.
struct ContainerPython(ContainerProcess);

impl PythonProcess for ContainerPython {
    fn exchange(&mut self, line: &str) -> Result<String, String> {
        self.0
            .exchange_with_timeout(line, HELPER_DEADLINE)
            .map_err(|error| error.to_string())
    }

    fn stop(&mut self) {
        self.0.stop();
    }

    fn shutdown(&mut self) {
        let _ = self
            .0
            .exchange_with_timeout(r#"{"op":"stop"}"#, Duration::from_secs(10));
        self.0.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn isolates_helper_names() {
        let names: std::collections::HashSet<_> = (0..1000)
            .flat_map(|_| [container_name("helper"), container_name("prepare")])
            .collect();
        assert_eq!(names.len(), 2000);
    }

    #[test]
    fn rejects_cancelled_starters() {
        let folder = tempfile::tempdir().unwrap();
        let cache = folder.path().join("cache");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let starter = ContainerStarter::with_cancel(
            PythonContainers {
                engine: "unreachable".into(),
                image: None,
                cache: cache.clone(),
                show_output: false,
            },
            Vec::new(),
            cancel,
        );
        let start = PythonStart {
            script: "",
            packages: Vec::new(),
            constraints: Vec::new(),
            cwd: folder.path().to_path_buf(),
        };
        assert!(starter.start(&start).err().unwrap().contains("cancelled"));
        assert!(!cache.exists());
    }

    #[test]
    fn cancels_environment_wait() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("environments")).unwrap();
        let key = fingerprint(&[DEFAULT_IMAGE, PREPARE, "", ""]);
        let lock =
            std::fs::File::create(folder.path().join(format!("environments/{key:016x}.lock")))
                .unwrap();
        lock.lock().unwrap();
        let cancel = CancellationToken::new();
        let starter = ContainerStarter::with_cancel(
            PythonContainers {
                engine: "unreachable".into(),
                image: None,
                cache: folder.path().to_path_buf(),
                show_output: false,
            },
            Vec::new(),
            cancel.clone(),
        );
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let began = Instant::now();
        assert!(
            starter
                .prepared_environment(&[], &[], None)
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(began.elapsed() < Duration::from_secs(2));
        canceller.join().unwrap();
    }

    #[test]
    fn forwards_transitive_constraints() {
        let folder = tempfile::tempdir().unwrap();
        let starter = ContainerStarter::new(
            PythonContainers {
                engine: "unreachable".into(),
                image: None,
                cache: folder.path().to_path_buf(),
                show_output: false,
            },
            Vec::new(),
        );
        let packages = vec!["first==1".to_owned()];
        let mut paths = Vec::new();
        for transitive in ["second==1", "second==2"] {
            let constraints = vec!["first==1".to_owned(), transitive.to_owned()];
            let key = fingerprint(&[DEFAULT_IMAGE, PREPARE, "first==1", &constraints.join("\n")]);
            let expected = folder.path().join(format!("environments/{key:016x}"));
            std::fs::create_dir_all(&expected).unwrap();
            let unordered = vec![
                constraints[1].clone(),
                constraints[0].clone(),
                constraints[1].clone(),
            ];
            let actual = starter
                .prepared_environment(&packages, &unordered, None)
                .unwrap();
            assert_eq!(actual, expected);
            paths.push(actual);
            let environment = starter.environment(&constraints);
            let json = &environment
                .iter()
                .find(|(name, _)| name == "UWE_PYTHON_CONSTRAINTS")
                .unwrap()
                .1;
            assert_eq!(
                serde_json::from_str::<Vec<String>>(json).unwrap(),
                constraints
            );
        }
        assert_ne!(paths[0], paths[1]);
    }

    #[test]
    fn avoids_nested_mounts() {
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
