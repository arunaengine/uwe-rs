//! One attempt of a task as a backend runs it: the specification, the inputs to place, and what
//! the task left behind.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use task_model::{Digest, Placement, TaskSpec};

use crate::manifest::{Collected, ManifestError, parse};
use crate::wrapper::{END, EXIT, MANIFEST, ROOT, START, STDERR, STDOUT, USAGE};

/// The folders of a task's scratch directory: the work directory, the control files of the
/// wrapper, the home folder of the task, and copies of inputs at absolute paths, which are
/// mounted where the task sees them.
pub const WORK: &str = "work";
pub const CONTROL: &str = "control";
pub const HOME: &str = "home";
pub(crate) const MOUNTS: &str = "mounts";
/// The control file where a remote stager lists the outputs that it put into the content store
/// itself, as a JSON list of [`OutputReceipt`]s.
pub const RECEIPTS: &str = "receipts";

/// One attempt of a task.
#[derive(Clone, Copy, Debug)]
pub struct Job<'a> {
    /// A name no other attempt uses, like `<run>-<task>-<attempt>`. The scratch directory and
    /// the container are named after it.
    pub name: &'a str,
    pub spec: &'a TaskSpec,
    pub staging: &'a Staging,
    pub placement: &'a Placement,
}

/// An input placed before the task starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staged {
    /// A path relative to the work directory, or an absolute path inside the container.
    pub path: String,
    /// The local file or directory that it shows.
    pub source: PathBuf,
    pub directory: bool,
    /// Whether the task gets a copy of its own, because it may change the input or run it.
    /// Other inputs are shown read-only: as a link to the source, which the container sees at
    /// the same path.
    pub copy: bool,
    /// Whether a copy is a program the task runs.
    pub executable: bool,
    /// Whether the task changes the source where it is: the container sees it writable at the
    /// path of the input, and later tasks see the changes.
    pub in_place: bool,
}

/// What a backend places before a task starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staging {
    pub inputs: Vec<Staged>,
    /// Directories to make, like the empty directories of a directory input.
    pub directories: Vec<String>,
    /// The local file that the task reads on standard input.
    pub stdin: Option<PathBuf>,
    /// Input files that the workers fetch where they lie; only a backend whose
    /// [`crate::Backend::direct_sources`] says so gets them.
    pub direct: Vec<DirectSource>,
    /// Where a backend that can sends the output and resource use of the attempt while it runs.
    pub live: Option<crate::live::LiveSink>,
}

/// An input file that the worker of a remote backend fetches itself, so that its bytes never
/// pass through this machine. A host backend decides from it where the task runs best.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectSource {
    /// A path relative to the work directory, or an absolute path inside the container.
    pub path: String,
    /// The address of the input as the workflow named it, like `aruna://object/<id>`; a file
    /// of a directory input has the directory's address with its path appended.
    pub source: String,
    /// The verified digest of its content, which the worker checks.
    pub digest: Digest,
    pub size: u64,
    pub executable: bool,
    /// Where the worker reads it.
    pub reference: storage::DirectRef,
    /// The endpoints that hold the bytes, the nearest first, as the storage provider says.
    pub endpoints: Vec<String>,
}

/// An output that the worker or its stager put into the content store of the run itself, so
/// that its bytes never pass through this machine.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputReceipt {
    /// The path of the output as the task named it, as in [`Finished::files`].
    pub path: String,
    pub digest: Digest,
    pub size: u64,
    /// Where the stager put it: a location of the store, like the one of its content, or an
    /// address that records where else it went.
    pub location: String,
}

/// What a task left behind in its scratch directory.
#[derive(Debug)]
pub struct Finished {
    /// The tool's exit code, or the container's when it was stopped before the tool ended,
    /// like 137 for a container killed for lack of memory. `None` when neither is known.
    pub exit_code: Option<i32>,
    /// The collected outputs, by the paths the task named them with.
    pub files: Vec<Collected>,
    /// The scratch directory, which the caller removes once it has the outputs.
    pub root: PathBuf,
    /// What the task used, when its container had a cgroup of its own.
    pub usage: Option<Usage>,
    /// Where other tasks reach a daemon task that is ready; it runs on until it is stopped.
    pub address: Option<String>,
    /// The outputs that the backend put into the content store itself, which the runner
    /// records without reading their bytes.
    pub receipts: Vec<OutputReceipt>,
}

/// What a task used, as the cgroup of its container counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// The CPU time, in microseconds.
    pub cpu_usec: Option<u64>,
    /// The peak of memory, in bytes, page cache included.
    pub memory_peak: Option<u64>,
    /// The bytes read from and written to block devices.
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    /// How many processes of the task the kernel killed for lack of memory.
    pub oom_kills: Option<u64>,
    /// The largest memory of the cgroup that the wrapper sampled while the task ran, page
    /// cache included, in bytes.
    pub vmem: Option<u64>,
    /// The largest anonymous memory of the cgroup that it sampled, in bytes.
    pub rss: Option<u64>,
    /// The peak of the cgroup's memory at the last sample, in bytes.
    pub peak_vmem: Option<u64>,
    /// The memory of the machine, in bytes, as `MemTotal` of `/proc/meminfo` shows it.
    pub memory_total: Option<u64>,
}

impl Usage {
    /// What the wrapper recorded in the control folder of a scratch directory, if anything.
    pub fn of(root: &std::path::Path) -> Option<Usage> {
        let text = std::fs::read_to_string(root.join(CONTROL).join(USAGE)).ok()?;
        let mut usage = Usage::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            let value = value.trim().parse().ok();
            match key {
                "cpu_usec" => usage.cpu_usec = value,
                "memory_peak" => usage.memory_peak = value,
                "read_bytes" => usage.read_bytes = value,
                "write_bytes" => usage.write_bytes = value,
                "oom_kills" => usage.oom_kills = value,
                "vmem" => usage.vmem = value,
                "rss" => usage.rss = value,
                "peak_vmem" => usage.peak_vmem = value,
                "memory_total" => usage.memory_total = value,
                _ => {}
            }
        }
        (usage != Usage::default()).then_some(usage)
    }
}

impl Finished {
    /// Adds the files selected by the task's optional JSON output manifest.
    pub fn collect_outputs(
        &mut self,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<(), ExecutorError> {
        if self.exit_code == Some(0)
            && let Some(discovery) = crate::manifest::Discovery::of(spec, staging)
        {
            discovery
                .collect_staged(&self.root, &mut self.files, staging)
                .map_err(ExecutorError::Unsupported)?;
        }
        Ok(())
    }

    /// When the tool of the task started and ended, as the wrapper wrote it in the control
    /// folder: seconds since the Unix epoch, with a fraction where Bash has one. `None` when
    /// the tool did not get that far.
    pub fn tool_times(&self) -> Option<(SystemTime, SystemTime)> {
        let control = self.root.join(CONTROL);
        let read = |name: &str| -> Option<SystemTime> {
            let text = std::fs::read_to_string(control.join(name)).ok()?;
            // Bash writes the fraction with the decimal separator of the locale.
            let seconds: f64 = text.trim().replace(',', ".").parse().ok()?;
            let elapsed = Duration::try_from_secs_f64(seconds).ok()?;
            SystemTime::UNIX_EPOCH.checked_add(elapsed)
        };
        let (started, ended) = (read(START)?, read(END)?);
        (ended >= started).then_some((started, ended))
    }

    /// Where an output is on the host: a relative path in the work directory, an absolute one
    /// where the wrapper copied it.
    pub fn local_path(&self, path: &str) -> PathBuf {
        match path.starts_with('/') {
            true => self
                .root
                .join(CONTROL)
                .join(ROOT)
                .join(path.trim_start_matches('/')),
            false => self.root.join(WORK).join(path),
        }
    }

    /// What the wrapper left in the control folder of a scratch directory: the tool's exit
    /// code, and after a success the manifest of its outputs. For backends whose server gives
    /// the control folder back, like TES.
    pub fn from_control(root: PathBuf) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let read = |name: &str| std::fs::read_to_string(control.join(name)).ok();
        let exit_code = read(EXIT).and_then(|text| text.trim().parse().ok());
        let files = match exit_code {
            Some(0) => parse(&read(MANIFEST).unwrap_or_default())?,
            _ => Vec::new(),
        };
        Ok(Finished {
            address: None,
            exit_code,
            files,
            usage: Usage::of(&root),
            root,
            receipts: Vec::new(),
        })
    }

    /// Reads the receipts that a stager left in the control folder after a success, for the
    /// output files of the manifest. Only a backend whose own stager writes them calls this,
    /// as the task can write a receipt into its control folder as well.
    pub fn read_receipts(&mut self) -> Result<(), ExecutorError> {
        let text = std::fs::read_to_string(self.root.join(CONTROL).join(RECEIPTS)).ok();
        if let (Some(0), Some(text)) = (self.exit_code, text) {
            self.receipts = receipts(&text, &self.files)?;
        }
        Ok(())
    }

    /// The receipt of an output file that the backend put into the content store itself.
    pub fn receipt(&self, path: &str) -> Option<&OutputReceipt> {
        self.receipts.iter().find(|receipt| receipt.path == path)
    }

    /// The file that captured standard output.
    pub fn stdout(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDOUT)
    }

    /// The file that captured standard error.
    pub fn stderr(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDERR)
    }
}

/// The receipts of a stager, each for one output file of the manifest.
fn receipts(text: &str, files: &[Collected]) -> Result<Vec<OutputReceipt>, ExecutorError> {
    let invalid = |why: &str| ExecutorError::Unsupported(format!("invalid output receipts: {why}"));
    let receipts: Vec<OutputReceipt> =
        serde_json::from_str(text).map_err(|error| invalid(&error.to_string()))?;
    for receipt in &receipts {
        let listed = (files.iter()).any(|file| !file.directory && file.path == receipt.path);
        if !listed || receipt.location.is_empty() {
            return Err(invalid("a receipt names no output file of the manifest"));
        }
        let location = &receipt.location;
        let escapes = location.starts_with('/') || location.split('/').any(|part| part == "..");
        if !location.contains("://") && escapes {
            return Err(invalid("a receipt names a location outside the store"));
        }
    }
    Ok(receipts)
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("the executor does not support this task: {0}")]
    Unsupported(String),
    #[error("the task has no container image; every task runs in a container")]
    NoImage,
    #[error("the task has no command, and the image {0} has no default command")]
    NoCommand(String),
    /// The registry does not have the image, or does not let this machine pull it: another
    /// attempt would not work either.
    #[error(
        "the image {image} cannot be pulled: {reason}; check its name and tag, or log in to its \
         registry"
    )]
    MissingImage { image: String, reason: String },
    /// The image has no `sh`, which runs the wrapper of every task.
    #[error("the image {0} has no sh, which uwe needs to run a task; use an image with a shell")]
    NoShell(String),
    #[error("cannot prepare the task directory: {0}")]
    Io(#[from] std::io::Error),
    /// The container engine or the machine failed, not the tool; another attempt may work.
    #[error("{engine} could not run the task: {message}")]
    Engine { engine: String, message: String },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the task was cancelled")]
    Cancelled,
}

impl ExecutorError {
    /// Whether the machine failed rather than the task, so that another attempt may work.
    pub fn retryable(&self) -> bool {
        matches!(self, ExecutorError::Engine { .. })
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn reads_control_usage() {
        let root = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(root.path().join(CONTROL)).expect("makes the folder");
        assert_eq!(Usage::of(root.path()), None);
        let text = "vmem 4096\nrss 1024\npeak_vmem 8192\nmemory_total 65536\nmemory_peak 2048\n\
                    oom_kills 1\ncpu_usec 1500\nread_bytes 0\nwrite_bytes 4096\n";
        std::fs::write(root.path().join(CONTROL).join(USAGE), text).expect("writes");
        let usage = Usage::of(root.path()).expect("a usage");
        let sampled = (usage.vmem, usage.rss, usage.peak_vmem, usage.memory_total);
        assert_eq!(sampled, (Some(4096), Some(1024), Some(8192), Some(65536)));
        assert_eq!(usage.memory_peak, Some(2048));
        assert_eq!(usage.cpu_usec, Some(1500));
        assert_eq!(usage.write_bytes, Some(4096));
        assert_eq!(usage.oom_kills, Some(1));
    }

    #[test]
    fn reads_output_receipts() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let control = root.path().join(CONTROL);
        std::fs::create_dir_all(&control).expect("makes the folder");
        std::fs::write(control.join(EXIT), "0\n").expect("writes");
        std::fs::write(control.join(MANIFEST), "file out.bam\ndirectory logs\n").expect("writes");
        let digest = Digest::sha256(b"bam");
        let receipt =
            format!(r#"[{{"path":"out.bam","digest":"{digest}","size":3,"location":"cas/x"}}]"#);
        std::fs::write(control.join(RECEIPTS), &receipt).expect("writes");
        let read = |root: &std::path::Path| {
            let mut finished = Finished::from_control(root.to_owned())?;
            finished.read_receipts().map(|_| finished)
        };
        // The control folder alone, which the task can write, gives no receipts.
        let finished = Finished::from_control(root.path().to_owned()).expect("a result");
        assert!(finished.receipts.is_empty());
        let finished = read(root.path()).expect("a result");
        let found = finished.receipt("out.bam").expect("a receipt");
        assert_eq!((&found.digest, found.size), (&digest, 3));
        assert!(finished.receipt("logs").is_none());
        // A receipt must name an output file of the manifest.
        let stray = receipt.replace("out.bam", "logs");
        std::fs::write(control.join(RECEIPTS), stray).expect("writes");
        assert!(read(root.path()).is_err());
        // A location outside the store is refused.
        for place in ["/etc/passwd", "cas/../../x", ".."] {
            let outside = receipt.replace("cas/x", place);
            std::fs::write(control.join(RECEIPTS), outside).expect("writes");
            assert!(read(root.path()).is_err(), "{place}");
        }
        // A failed task gives no receipts.
        std::fs::write(control.join(RECEIPTS), &receipt).expect("writes");
        std::fs::write(control.join(EXIT), "1\n").expect("writes");
        let failed = read(root.path()).expect("a result");
        assert!(failed.receipts.is_empty());
    }

    #[test]
    fn reads_control_times() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let control = root.path().join(CONTROL);
        std::fs::create_dir_all(&control).expect("makes the folder");
        let finished = Finished {
            address: None,
            exit_code: Some(0),
            files: Vec::new(),
            root: root.path().to_owned(),
            usage: None,
            receipts: Vec::new(),
        };
        assert_eq!(finished.tool_times(), None);
        std::fs::write(control.join(START), "1700000000.250000\n").expect("writes");
        std::fs::write(control.join(END), "1700000002,750000\n").expect("writes");
        let (started, ended) = finished.tool_times().expect("times");
        let seconds = |time: SystemTime| {
            let elapsed = time.duration_since(SystemTime::UNIX_EPOCH);
            elapsed.expect("after 1970").as_secs_f64()
        };
        assert_eq!(seconds(started), 1_700_000_000.25);
        assert_eq!(seconds(ended), 1_700_000_002.75);
        std::fs::write(control.join(END), "1699999999\n").expect("writes");
        assert_eq!(finished.tool_times(), None, "an end before the start");
    }
}
