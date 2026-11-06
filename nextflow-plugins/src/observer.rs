//! The run events that observer plugins hear, and a worker thread for observers that send them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Hears the events of a real run, in the order they happen, like Nextflow's trace observers.
/// A simulated run has no observers.
pub trait Observer {
    fn notify(&mut self, event: &Event);

    /// Checks a run whose tasks all succeeded, before [`Event::RunEnded`]. An error fails the
    /// run with its message, as nf-snapshot fails a run whose files differ from its snapshot.
    fn check(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// One event of a run.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    RunStarted(Box<Start>),
    /// The run handed a task to the engine. A task that the cache answers on resume has this
    /// event too, then [`Event::TaskCached`].
    TaskSubmitted(Task),
    /// An attempt began: the executor has it.
    TaskStarted(Task),
    TaskCompleted(Task),
    /// An earlier result answered the task, so it did not run.
    TaskCached(Task),
    /// An attempt ended with an error, also one that the error strategy retries or ignores.
    TaskFailed(Task),
    /// A file went to where the workflow publishes it, like a `publishDir`.
    FilePublished {
        source: String,
        target: String,
    },
    /// The run ended; observers hear nothing after it.
    RunEnded(End),
}

/// A task's trace record by Nextflow's trace field names, like `task_id`, `name`, `process`,
/// `status`, `exit`, `submit` and `realtime`. Times are milliseconds since 1970, durations
/// milliseconds and sizes bytes; a value that is not known is null.
pub type Record = serde_json::Map<String, serde_json::Value>;

/// A task as its events show it: the trace record and what the trace does not hold.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub record: Record,
    /// The number of its process in [`Start::processes`].
    pub process_id: Option<u32>,
    /// The SHA-256 by which a resumed run finds the task in the cache, in hex; the record's
    /// `hash` has Nextflow's short form of it, like `ab/cdef01`.
    pub hash: String,
    /// The `executor` directive of the process, `local` when it names none, as Nextflow names
    /// executors; uwe's own executor may differ.
    pub executor: String,
    /// The files staged for the task.
    pub inputs: Vec<File>,
    /// The files that the task made; empty before it ended.
    pub outputs: Vec<File>,
    /// The values of its inputs, one per input declaration; a tuple is a list.
    pub values: Vec<serde_json::Value>,
    /// The standard output and error of an attempt that ended, as far as the executor kept
    /// them; empty before it ended.
    pub stdout: String,
    pub stderr: String,
}

/// A file of a task.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    /// The path that the task sees, relative to its work directory.
    pub name: String,
    /// Where the file is: a path on this machine or an address like `s3://bucket/key`.
    pub path: String,
    /// The content digest, like `sha256:<hex>`, when the executor computed it.
    pub digest: Option<String>,
}

/// The start of a run, as Nextflow's `workflow` object shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    pub run_name: String,
    pub session_id: String,
    pub launch_dir: String,
    /// The work directory, as `workflow.workDir` shows it.
    pub work_dir: String,
    pub project_dir: String,
    /// The main script that runs, after `manifest.mainScript`.
    pub script: String,
    /// `owner/name` of a pipeline pulled from a repository, else the folder of the project.
    pub project_name: String,
    /// The address of the Git repository of a pulled pipeline, with its commit and the
    /// revision asked for; `None` for a local pipeline, as in Nextflow.
    pub repository: Option<String>,
    pub commit_id: Option<String>,
    pub revision: Option<String>,
    pub command_line: String,
    pub profile: String,
    /// The config files that were read, in order.
    pub config_files: Vec<String>,
    pub params: serde_json::Value,
    /// The merged config as nested objects.
    pub config: serde_json::Value,
    /// The `manifest` scope of the config, like `name`, `version` and `homePage`.
    pub manifest: serde_json::Value,
    /// The processes that the workflow calls, numbered from 1 in the order of the calls.
    pub processes: Vec<Process>,
    /// Milliseconds since 1970.
    pub start: i64,
    /// Whether the run resumes an earlier one, with `-resume`, as `workflow.resume` shows it.
    pub resume: bool,
}

/// A process of the run, a vertex of the process graph that Nextflow's DAG shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub id: u32,
    /// The fully qualified name, like `NFCORE_DEMO:DEMO:FASTQC`.
    pub name: String,
    /// The name of the definition, which differs from the end of `name` for an alias.
    pub definition: String,
    /// The script file that defines it, whose folder is the module directory.
    pub script: Option<String>,
    /// The processes that its outputs reach through channels and operators, by id.
    pub feeds: Vec<u32>,
}

/// The end of a run.
#[derive(Clone, Debug, PartialEq)]
pub struct End {
    pub success: bool,
    /// The message of the error that ended a failed run.
    pub error: Option<String>,
    pub counts: Counts,
    /// The task whose failure ended a failed run, if one did.
    pub task: Option<Task>,
    /// The workflow outputs of the `output {}` block.
    pub outputs: Vec<Output>,
    /// Milliseconds since 1970.
    pub end: i64,
}

/// A workflow output with the files that it published.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    /// The targets of its files, in the order they were published.
    pub files: Vec<String>,
    /// Its index file, when it has one.
    pub index: Option<String>,
}

/// How the attempts of a run ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub succeeded: u64,
    pub cached: u64,
    pub failed: u64,
}

/// An observer that hands each event to a worker thread, so that one that talks to a network
/// service never stalls the run. See the README of this crate for its use.
pub struct Background {
    sender: Option<mpsc::Sender<Event>>,
    worker: Option<JoinHandle<()>>,
    /// Hears from the worker when it handled the last event.
    finished: mpsc::Receiver<()>,
    /// How long the end of the run waits for the worker to handle the events that are left.
    wait: Duration,
}

impl Background {
    /// Starts the worker thread `name`, which calls `handle` with each event in order.
    pub fn start(
        name: &str,
        wait: Duration,
        mut handle: impl FnMut(Event) + Send + 'static,
    ) -> std::io::Result<Background> {
        Background::spawn(name, wait, None, move |event| {
            if let Some(event) = event {
                handle(event);
            }
        })
    }

    /// Like [`Background::start`], but `handle` also hears `None` when `tick` passed without
    /// an event, for observers that send on a timer, like heartbeats.
    pub fn ticking(
        name: &str,
        wait: Duration,
        tick: Duration,
        handle: impl FnMut(Option<Event>) + Send + 'static,
    ) -> std::io::Result<Background> {
        Background::spawn(name, wait, Some(tick), handle)
    }

    fn spawn(
        name: &str,
        wait: Duration,
        tick: Option<Duration>,
        mut handle: impl FnMut(Option<Event>) + Send + 'static,
    ) -> std::io::Result<Background> {
        let (sender, events) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                loop {
                    let event = match tick {
                        Some(tick) => match events.recv_timeout(tick) {
                            Ok(event) => Some(event),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        },
                        None => match events.recv() {
                            Ok(event) => Some(event),
                            Err(mpsc::RecvError) => break,
                        },
                    };
                    handle(event);
                }
                let _ = done.send(());
            })?;
        Ok(Background {
            sender: Some(sender),
            worker: Some(worker),
            finished,
            wait,
        })
    }

    /// Ends the queue and waits at most `wait` for the worker; a worker that takes longer is
    /// left behind, so that a slow service cannot hold the end of the run.
    fn flush(&mut self) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        drop(sender);
        match self.finished.recv_timeout(self.wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(wait = ?self.wait, "a run observer did not finish in time");
            }
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(worker) = self.worker.take()
                    && worker.join().is_err()
                {
                    tracing::warn!("a run observer stopped with a panic");
                }
            }
        }
    }
}

impl Observer for Background {
    fn notify(&mut self, event: &Event) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event.clone());
        }
        if let Event::RunEnded(_) = event {
            self.flush();
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn ended() -> Event {
        Event::RunEnded(End {
            success: true,
            error: None,
            counts: Counts::default(),
            task: None,
            outputs: Vec::new(),
            end: 0,
        })
    }

    #[test]
    fn flushes_at_end() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let mut observer = Background::start("test", Duration::from_secs(60), move |event| {
            kept.lock().expect("not poisoned").push(event);
        })
        .expect("starts the worker");
        let published = Event::FilePublished {
            source: "/work/a".into(),
            target: "/results/a".into(),
        };
        observer.notify(&published);
        observer.notify(&ended());
        observer.notify(&published);
        let heard = heard.lock().expect("not poisoned");
        assert_eq!(*heard, [published, ended()]);
    }

    #[test]
    fn ticks_when_quiet() {
        let (heard, ticks) = mpsc::channel();
        let mut observer = Background::ticking(
            "tick",
            Duration::from_secs(60),
            Duration::from_millis(1),
            move |event| {
                let _ = heard.send(event.is_none());
            },
        )
        .expect("starts the worker");
        assert_eq!(ticks.recv_timeout(Duration::from_secs(60)), Ok(true));
        observer.notify(&ended());
        assert!(ticks.iter().any(|tick| !tick), "the end is heard too");
    }

    #[test]
    fn leaves_slow_worker() {
        let (release, blocked) = mpsc::channel::<()>();
        let mut observer = Background::start("slow", Duration::from_millis(1), move |_| {
            let _ = blocked.recv();
        })
        .expect("starts the worker");
        observer.notify(&ended());
        assert!(observer.sender.is_none());
        assert!(observer.worker.is_some(), "the end does not wait for it");
        drop(release);
    }
}
