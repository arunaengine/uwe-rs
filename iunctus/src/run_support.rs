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
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    let previous = LAST
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
            Some(now.max(previous.saturating_add(1)))
        })
        .expect("the update always succeeds");
    let now = now.max(previous.saturating_add(1));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure;
    use workflow_graph::{DriverError, Engine, Launch};

    #[test]
    #[ignore = "needs Docker and the cached debian:stable-slim image"]
    fn restricted_inputs_readable() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("private.txt");
        std::fs::write(&source, "private input\n").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        for uid in [0, 65534] {
            let target = root.path().join("workflow.yaml");
            std::fs::write(&target, format!(
                "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: read}}\nspec:\n\
                 \x20 entrypoint: read\n  volumes:\n  - name: source\n    hostPath: {{path: '{}', type: File}}\n\
                 \x20 templates:\n  - name: read\n    container:\n      image: debian:stable-slim\n\
                 \x20     command: [sh, -c]\n      args: ['cat /data/private.txt > /tmp/read.txt']\n\
                 \x20     securityContext: {{runAsUser: {uid}, runAsGroup: {uid}}}\n\
                 \x20     volumeMounts: [{{name: source, mountPath: /data/private.txt, readOnly: true}}]\n\
                 \x20   outputs:\n      parameters: [{{name: value, valueFrom: {{path: /tmp/read.txt}}}}]\n",
                source.display(),
            )).unwrap();
            let run = format!("permissions-{uid}");
            let store = content_store::Store::local(&root.path().join("store")).unwrap();
            let runtime = workflow_graph::runtime().unwrap();
            let engine = runtime
                .block_on(executor::ContainerEngine::named(Some("docker")))
                .unwrap();
            let backend = executor::LocalContainer::new(
                engine,
                root.path().join("scratch"),
                executor::Pool::new(2, 1 << 30),
            );
            let settings = task_runner::Settings {
                downloads: root.path().join("downloads"),
                import_inputs: false,
                storage: storage::Storage::default(),
                digests: None,
            };
            let mut executor = task_runner::RunnerExecutor::new(
                task_runner::Runner::new(store, backend, &run, settings),
                None,
            );
            let launch = Launch {
                target,
                scratch: root.path().join("planning"),
                ..Launch::default()
            };
            let mut driver = argo_runtime::driver(&launch).unwrap();
            let execution = Engine::new(&mut executor).run("permissions", driver.as_mut());
            assert!(
                execution.error.is_none(),
                "uid {uid}: {:?}",
                execution.error
            );
            assert_eq!(execution.graph.nodes.len(), 1);
            assert_eq!(execution.graph.nodes[0].exit_code, Some(0));
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "private input\n");
            assert_eq!(
                std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn preserves_hook_exit() {
        let execution = Execution {
            graph: ExecutionGraph::default(),
            error: Some(workflow_graph::ExecuteError::Machine {
                task: "task".into(),
                message: "original machine failure".into(),
            }),
            failure_hook_error: Some(DriverError::unsupported("failure hook problem")),
        };
        let failure = finish(execution).expect_err("failed execution");
        assert_eq!(failure.code, failure::SETUP);
        assert!(failure.message.contains("original machine failure"));
        assert!(
            failure
                .after
                .iter()
                .any(|note| note.contains("failure hook problem"))
        );
    }
}
