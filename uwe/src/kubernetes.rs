//! Builds Kubernetes task executors with explicit kubeconfig, and shared host paths or a staging
//! root.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use content_store::Store;
use executor::orphans::Orphan;
use kubernetes_executor::{Cluster, KubernetesBackend, PodStaging, Via, Volumes};
use task_runner::{Runner, RunnerExecutor};

use crate::Failure;
use crate::arguments::Arguments;
use crate::setup::Setup;

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
    let backend = backend(scratch, run, kubeconfig, namespace, setup)?;
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    runtime
        .block_on(backend.reconcile())
        .map_err(|error| error.to_string())?;
    // The nodes read the inputs from the store, so they are copied there.
    let backend = crate::conda_backend::Remote::new(backend, store.clone(), scratch, run, setup);
    let mut runner = Runner::new(
        store,
        backend,
        run,
        crate::runner_settings(setup.storage.clone(), true),
    );
    runner.references(setup.references.clone());
    Ok(RunnerExecutor::new(runner, default_image))
}

/// The Kubernetes Jobs that the run `run` left, from its journals in the scratch folder of
/// this machine: their state only, or, with `stop`, after this call deleted them.
pub fn orphans(
    arguments: &Arguments,
    setup: &Setup,
    run: &str,
    stop: bool,
) -> Result<Vec<Orphan>, Failure> {
    let failed = |message: String| Failure::setup(format!("uwe: {message}"));
    let scratch = crate::cache_directory().join("scratch");
    let journals = executor::orphans::journals(&scratch.join("kubernetes"), run);
    // Without a journal no cluster needs to answer.
    if journals
        .map_err(|error| failed(error.to_string()))?
        .is_empty()
    {
        return Ok(Vec::new());
    }
    let settings = &setup.executors;
    let kubeconfig = (arguments.kubeconfig.as_deref()).or(settings.kubeconfig.as_deref());
    let namespace =
        (arguments.kubernetes_namespace.clone()).or_else(|| settings.kubernetes_namespace.clone());
    let backend = backend(&scratch, run, kubeconfig, namespace, setup).map_err(failed)?;
    let runtime = workflow_graph::runtime().map_err(|error| failed(error.to_string()))?;
    let found = match stop {
        true => runtime.block_on(backend.stop_run(run)),
        false => runtime.block_on(backend.report_run(run)),
    };
    found.map_err(|error| failed(error.to_string()))
}

/// The backend of the run `run` in the cluster of `kubeconfig`, with the staging of the config
/// file, before it recovers the attempts of other runs.
fn backend(
    scratch: &Path,
    run: &str,
    kubeconfig: Option<&Path>,
    namespace: Option<String>,
    setup: &crate::setup::Setup,
) -> Result<KubernetesBackend, String> {
    // The client runs on the runtime of the engine, which runs the attempts.
    let runtime = workflow_graph::runtime().map_err(|error| error.to_string())?;
    // The first line says what is wrong; the lines after it quote the file.
    let cluster = match &setup.cluster {
        Some(cluster) => cluster.clone(),
        None if setup.executors.kubernetes_in_cluster => {
            runtime.block_on(async { Cluster::in_cluster(namespace) })?
        }
        None => {
            let kubeconfig = kubeconfig.ok_or("-executor kubernetes requires -kubeconfig, an injected client or explicit in_cluster configuration")?;
            let context = setup.executors.kubernetes_context.clone();
            runtime.block_on(Cluster::connect_context(kubeconfig, namespace, context))?
        }
    };
    let mut backend = KubernetesBackend::new(cluster, scratch.join("kubernetes"));
    backend.run = run.to_owned();
    let executors = &setup.executors;
    backend.trusted = !setup.hosted;
    backend.references = executors.kubernetes_references.clone();
    backend.pod = executors.kubernetes_pod.clone();
    if let Some(cws) = &executors.kubernetes_cws {
        let scheduler = serde_json::json!({"schedulerName": cws.scheduler_name});
        backend.pod.add(&scheduler)?;
        backend.cws = Some(kubernetes_executor::cws::Client::new(cws.clone(), run));
    }
    if let Volumes::Under { root, .. } = &executors.kubernetes_volumes
        && !backend.scratch.starts_with(root)
    {
        return Err(format!(
            "the scratch folder {} is not in the shared volume at {}; keep the cache of uwe \
             there",
            backend.scratch.display(),
            root.display()
        ));
    }
    backend.volumes = executors.kubernetes_volumes.clone();
    backend.staging_time = executors.kubernetes_staging.margin();
    if let Some(minutes) = executors.kubernetes_unschedulable {
        backend.unschedulable_time = std::time::Duration::from_secs(minutes * 60);
    }
    if let Some(seconds) = executors.kubernetes_start_grace {
        backend.start_grace = std::time::Duration::from_secs(seconds);
    }
    if let Some(stage) = executors.kubernetes_staging.stage(&setup.storage)? {
        let via = match (&executors.kubernetes_kubedal, &executors.kubernetes_helper) {
            (Some(kubedal), helper) => {
                let mut kubedal = kubedal.clone();
                let binary = executors
                    .kubernetes_helper_file
                    .as_ref()
                    .map(|file| executor::binary::Binary::open(file))
                    .transpose()
                    .map_err(|error| error.to_string())?;
                kubedal.helper = helper.clone().or_else(|| {
                    binary.as_ref().map(|_| kubernetes_executor::Helper {
                        image: kubernetes_executor::DOWNLOAD_IMAGE.into(),
                        image_volume: false,
                        secret: None,
                        binary: None,
                    })
                });
                if let Some(helper) = &mut kubedal.helper {
                    helper.binary = binary;
                }
                Via::Kubedal(kubedal)
            }
            (None, Some(helper)) => {
                let mut helper = helper.clone();
                if let Some(file) = &executors.kubernetes_helper_file {
                    let binary = executor::binary::Binary::open(file)
                        .map_err(|error| format!("{error}, or set helper_image"))?;
                    helper.binary = Some(binary);
                }
                Via::Helper(helper)
            }
            (None, None) => return Err("Kubernetes object-store staging needs helper_image".into()),
        };
        backend.remote = Some(PodStaging { stage, via });
    }
    Ok(backend)
}
