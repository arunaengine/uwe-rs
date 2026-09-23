//! Shared cache and task-runner setup.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::failure::{Failure, INTERRUPTED};
use std::path::PathBuf;
use workflow_graph::{CancellationToken, Execution, ExecutionGraph, SIMULATION};

/// How the task runner treats the inputs of tasks: remote files are downloaded once into the
/// cache folder, and the digests of local files are kept there, so that a file that did not
/// change is not read again. `import_inputs` copies local inputs into the store too, for
/// executors whose tasks run on other machines.
pub(crate) fn runner_settings(
    storage: storage::Storage,
    import_inputs: bool,
) -> task_runner::Settings {
    let cache = cache_directory();
    let digests = cache.join(task_runner::DigestCache::FILE_NAME);
    task_runner::Settings {
        downloads: cache.join("downloads"),
        import_inputs,
        storage,
        // Without the cache of digests, inputs are read as before; it only saves time.
        digests: task_runner::DigestCache::open(&digests).ok(),
    }
}

/// The cache folder that `[cache] folder` of the config file names, set once at the start.
static CACHE_FOLDER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Uses this cache folder for the whole command, in place of the one of `XDG_CACHE_HOME`.
pub(crate) fn use_cache_folder(folder: PathBuf) {
    let _ = CACHE_FOLDER.set(folder);
}

/// The folder of `[cache] folder` of the config file, else `$XDG_CACHE_HOME/iunctus`, else
/// `~/.cache/iunctus`.
pub(crate) fn cache_directory() -> PathBuf {
    if let Some(folder) = CACHE_FOLDER.get() {
        return folder.clone();
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("iunctus")
}

/// A run id from the clock and the process id.
pub(crate) fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("{now:x}-{:x}", std::process::id())
}
/// Ends a command with its run: the error of a failed run comes first, so that it is the first
/// line on standard error, then the note of a simulated run.
pub(crate) fn finish(execution: Execution) -> Result<(), Failure> {
    let mut notes = simulation_notes(&execution.graph);
    if let Some(error) = &execution.failure_hook_error {
        notes.insert(
            0,
            format!("iunctus: error while ending the failed workflow: {error}"),
        );
    }
    match execution.into_result() {
        Ok(_) => {
            for note in notes {
                eprintln!("{note}");
            }
            Ok(())
        }
        Err(error) => {
            let mut failure = Failure::from(error);
            failure.after = notes;
            Err(failure)
        }
    }
}

/// What a simulated run says on standard error, which keeps standard output free for the
/// graph: that it is a simulation, and how many points of its graph are marked.
fn simulation_notes(graph: &ExecutionGraph) -> Vec<String> {
    // A run that asked for no task, like one that stopped while it loaded, simulated nothing.
    if !graph.is_simulated() || graph.nodes.is_empty() {
        return Vec::new();
    }
    let mut notes = vec![format!("iunctus: {SIMULATION}")];
    notes.extend(graph.cache_note().map(|note| format!("iunctus: {note}")));
    match graph.marked().count() {
        0 => {}
        1 => notes.push(
            "iunctus: the graph marks 1 point where real data can add or remove tasks.".into(),
        ),
        count => notes.push(format!(
            "iunctus: the graph marks {count} points where real data can add or remove tasks."
        )),
    }
    notes
}

/// A token that fires on Ctrl-C or SIGTERM, so that the run stops its tasks cleanly, as a
/// SLURM job or a service manager stops a program with SIGTERM. A second one ends iunctus at
/// once.
pub(crate) fn stop_on_interrupt() -> CancellationToken {
    let token = CancellationToken::new();
    let stop = token.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).ok();
            let mut asked = async || {
                let terminated = async {
                    match terminate.as_mut() {
                        Some(terminate) => terminate.recv().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    interrupted = tokio::signal::ctrl_c() => interrupted.is_ok(),
                    _ = terminated => true,
                }
            };
            if asked().await && !stop.is_cancelled() {
                eprintln!("iunctus: stopping the running tasks; press Ctrl-C again to quit now");
                stop.cancel();
            }
            if asked().await {
                std::process::exit(INTERRUPTED.into());
            }
        });
    });
    token
}
