//! Builds Kubernetes task executors with explicit kubeconfig and shared host paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use kubernetes_executor::{Cluster, KubernetesBackend};
use task_runner::{Runner, RunnerExecutor};

/// An executor that runs each task as a Job in the cluster of `kubeconfig`, in `namespace` or
/// else the namespace of the kubeconfig's context. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    kubeconfig: Option<&Path>,
    namespace: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<RunnerExecutor<crate::conda_backend::Remote<KubernetesBackend>>, String> {
    let kubeconfig = kubeconfig.ok_or(
        "-executor kubernetes needs the kubeconfig of the cluster: name it with -kubeconfig \
         <file>; iunctus never takes the default one",
    )?;
    let kubeconfig = std::path::absolute(kubeconfig).map_err(|error| error.to_string())?;
    if !kubeconfig.is_file() {
        return Err(format!(
            "the kubeconfig {} does not exist",
            kubeconfig.display()
        ));
    }
    // The client runs on the runtime of the engine, which runs the attempts.
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    // The first line says what is wrong; the lines after it quote the file.
    let cluster = runtime
        .block_on(Cluster::connect(&kubeconfig, namespace))
        .map_err(|error| error.lines().next().unwrap_or_default().to_owned())?;
    let backend = KubernetesBackend::new(cluster, scratch.join("kubernetes"));
    runtime
        .block_on(backend.reconcile())
        .map_err(|error| error.to_string())?;
    // The nodes read the inputs from the store, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), scratch, run, setup);
    let runner = Runner::new(
        store,
        backend,
        run,
        crate::runner_settings(setup.storage.clone(), true),
    );
    Ok(RunnerExecutor::new(runner, default_image))
}
