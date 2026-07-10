//! The Kubernetes executor: each task runs as a Kubernetes Job through the task runner. The
//! nodes must see the store and the scratch directories at the same paths, as `hostPath`
//! volumes. The cluster is the one of the kubeconfig that `-kubeconfig` names: iunctus never
//! takes the default kubeconfig, so that no run reaches a cluster by accident.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use kubernetes_executor::{Cluster, KubernetesBackend};
use storage::Storage;
use task_runner::{Runner, RunnerExecutor, Settings};

/// An executor that runs each task as a Job in the cluster of `kubeconfig`, in `namespace` or
/// else the namespace of the kubeconfig's context. Tasks without an image get `default_image`.
pub fn executor(
    store: Store,
    scratch: &Path,
    run: &str,
    default_image: Option<String>,
    storage: Storage,
    kubeconfig: Option<&Path>,
    namespace: Option<String>,
) -> Result<RunnerExecutor<KubernetesBackend>, String> {
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
    let cluster = runtime.block_on(Cluster::connect(&kubeconfig, namespace))?;
    let backend = KubernetesBackend::new(cluster, scratch.join("kubernetes"));
    // The nodes read the inputs from the store, so they are copied there.
    let settings = Settings {
        downloads: crate::cache_directory().join("downloads"),
        import_inputs: true,
        storage,
    };
    let runner = Runner::new(store, backend, run, settings);
    Ok(RunnerExecutor::new(runner, default_image))
}
