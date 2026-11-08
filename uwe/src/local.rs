//! The local container executor: each task runs in Docker or Podman through the task runner,
//! which reuses results from the cache.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::{Holder, Store};
use executor::{ContainerEngine, LocalContainer, Pool};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor};

/// An executor whose results go to the store and whose tasks run in `scratch`, with the
/// container engine of this machine that `engine` names (`docker`, `podman` or a socket; empty
/// for Docker, else Podman). Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    engine: &str,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Local>, String> {
    // The client of the engine runs on the runtime of the engine, which runs the attempts.
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    let named = Some(engine).filter(|name| !name.is_empty());
    let found = runtime.block_on(ContainerEngine::named(named));
    let engine = found.map_err(|error| {
        format!(
            "cannot reach a Docker or Podman engine: {error}; use `-executor simulator` for a run without containers, or `uwe check-machine` to check the setup"
        )
    })?.with_journal_root(crate::cache_directory().join("containers"));
    let backend = LocalContainer::new(engine, scratch.to_owned(), pool(setup));
    let backend = crate::conda_backend::Local {
        backend,
        store: store.clone(),
        config: setup.conda.clone(),
        holder: Holder::new(run, "Conda image preparation"),
        preparing: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        secrets: setup.secrets.clone(),
        secret_store: crate::nextflow::secret_store(setup),
        variables: setup.variables.clone(),
    };
    // Remote inputs share the folder of downloads with the files that workflow code reads.
    let mut runner = Runner::new(store, backend, run, crate::runner_settings(storage, false));
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

/// The pool of the run: the share of the host's pool when it gives one, else a pool of
/// `local_cpus` and `local_memory`, or of all of this machine.
fn pool(setup: &crate::setup::Setup) -> Pool {
    if let Some(pool) = &setup.local_pool {
        return pool.for_run();
    }
    let capacity = Pool::of_this_machine();
    Pool::new(
        setup.executors.local_cpus.unwrap_or(capacity.cpus()),
        setup.executors.local_memory.unwrap_or(capacity.memory()),
    )
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// Whether a future is ready at once, without waiting for it.
    fn ready(future: impl Future) -> bool {
        let mut context = Context::from_waker(Waker::noop());
        matches!(pin!(future).poll(&mut context), Poll::Ready(_))
    }

    /// Two runs of a host that need four CPUs at once share its two: the second waits until
    /// the first gives its CPUs back, and a run that stops after a failure stops only itself.
    #[tokio::test]
    async fn shares_host_pool() {
        let setup = crate::setup::Setup {
            local_pool: Some(Pool::new(2, 1 << 30)),
            ..Default::default()
        };
        let (first, second) = (pool(&setup), pool(&setup));
        let taken = first.share(2, None, 0).await;
        assert!(!ready(second.share(2, None, 0)));
        drop(taken);
        assert!(ready(second.share(2, None, 0)));
        first.hold();
        assert!(ready(first.held()));
        assert!(!ready(second.held()));
        let own = pool(&crate::setup::Setup::default());
        assert_eq!(own.cpus(), Pool::of_this_machine().cpus());
    }
}
