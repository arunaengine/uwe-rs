//! Builds Kubernetes task executors with explicit kubeconfig, and shared host paths or a staging
//! root.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use kubernetes_executor::{Cluster, KubernetesBackend, PodStaging, Via, Volumes};
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
    let mut backend = KubernetesBackend::new(cluster, scratch.join("kubernetes"));
    let executors = &setup.executors;
    if let Volumes::Under { root, .. } = &executors.kubernetes_volumes
        && !backend.scratch.starts_with(root)
    {
        return Err(format!(
            "the scratch folder {} is not in the shared volume at {}; keep the cache of iunctus \
             there",
            backend.scratch.display(),
            root.display()
        ));
    }
    backend.volumes = executors.kubernetes_volumes.clone();
    if let Some(stage) = executors.kubernetes_staging.stage(&setup.storage)? {
        let via = match (&executors.kubernetes_kubedal, &executors.kubernetes_helper) {
            (Some(kubedal), _) => Via::Kubedal(kubedal.clone()),
            (None, Some(helper)) => Via::Helper(helper.clone()),
            (None, None) => return Err("Kubernetes object-store staging needs helper_image".into()),
        };
        backend.remote = Some(PodStaging { stage, via });
    }
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
