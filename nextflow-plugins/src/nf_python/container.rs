//! Runs one interpreter call in a container, with its folders mounted where they are on this
//! machine, for the plugins that run Python or R code: no interpreter runs on the host.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use executor::CancellationToken;
use executor::process::{ContainerSpec, Mount, run_with_limit};

/// How long one call may run.
const LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

/// Runs the program with its arguments, sends its standard output and error to two files (one
/// file when they are the same) and writes its exit code, or `missing`, to the status file.
const WRAPPER: &str = r#"program="$1"; status="$2"; out="$3"; err="$4"; shift 4
command -v "$program" >/dev/null 2>&1 || { echo missing >"$status"; exit 0; }
if [ "$out" = "$err" ]; then "$program" "$@" >"$out" 2>&1; else "$program" "$@" >"$out" 2>"$err"; fi
echo $? >"$status""#;

/// One call: a program inside an image, and where it reads and writes.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    /// The container engine as `UWE_ENGINE` names it; empty for the default.
    pub engine: String,
    pub image: String,
    pub program: String,
    pub arguments: Vec<String>,
    /// Mounted read-write at the same path inside the container.
    pub folders: Vec<PathBuf>,
    /// Further mounts, like those that a call asks for.
    pub mounts: Vec<Mount>,
    pub workdir: PathBuf,
    pub environment: Vec<(String, String)>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    /// Where the exit code goes; inside one of `folders`.
    pub status: PathBuf,
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited(i32),
    /// The image has no such program.
    Missing,
}

/// Runs the call to its end; an error means the container did not run.
pub(crate) fn run(call: &Call) -> Result<Ended, String> {
    let _ = std::fs::remove_file(&call.status);
    let text = |path: &PathBuf| path.to_string_lossy().into_owned();
    let mut command = vec![
        "-c".to_owned(),
        WRAPPER.to_owned(),
        "sh".to_owned(),
        call.program.clone(),
        text(&call.status),
        text(&call.stdout),
        text(&call.stderr),
    ];
    command.extend(call.arguments.iter().cloned());
    let mut environment = vec![("HOME".to_owned(), "/tmp".to_owned())];
    environment.extend(call.environment.iter().cloned());
    let spec = ContainerSpec {
        engine: call.engine.clone(),
        name: name(),
        image: call.image.clone(),
        user: std::fs::metadata(&call.workdir)
            .ok()
            .map(|found| format!("{}:{}", found.uid(), found.gid())),
        entrypoint: Some("/bin/sh".to_owned()),
        command,
        mounts: (call.folders.iter())
            .map(|folder| Mount {
                source: folder.clone(),
                target: text(folder),
                read_only: false,
            })
            .chain(call.mounts.iter().cloned())
            .collect(),
        workdir: Some(text(&call.workdir)),
        environment,
        memory: None,
        show_stderr: false,
    };
    run_with_limit(&spec, &CancellationToken::new(), LIMIT).map_err(|error| error.to_string())?;
    let status = std::fs::read_to_string(&call.status)
        .map_err(|error| format!("the container wrote no exit code: {error}"))?;
    let _ = std::fs::remove_file(&call.status);
    match status.trim() {
        "missing" => Ok(Ended::Missing),
        code => (code.parse().map(Ended::Exited))
            .map_err(|_| format!("the container wrote the exit code {code:?}")),
    }
}

/// A container name that no other call of this machine uses.
fn name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = (SystemTime::now().duration_since(UNIX_EPOCH))
        .unwrap_or_default()
        .as_nanos();
    format!(
        "uwe-plugin-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
