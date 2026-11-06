//! `uwe kuberun`: runs uwe itself in a pod of a Kubernetes cluster, whose tasks run as Jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use k8s_openapi::api::core::v1::{ConfigMap, Pod, ServiceAccount};
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, LogParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client};
use kubernetes_executor::Cluster;
use serde_json::{Value as Json, json};

use crate::arguments::Arguments;
use crate::{Failure, failure};

/// Where the ConfigMap with the config files appears in the pod.
const FILES: &str = "/etc/uwe";

/// The folder of the pod's own files without a volume mount.
const WORK: &str = "/work";

/// What the pod runs and with what, before anything is made in the cluster.
#[derive(Debug, PartialEq)]
struct Plan {
    name: String,
    namespace: String,
    image: String,
    command: Vec<String>,
    /// The entries of the ConfigMap: config files by their name in [`FILES`].
    files: BTreeMap<String, String>,
    /// Persistent volume claims and their paths; the first holds uwe's cache.
    mounts: Vec<(String, String)>,
    cpus: Option<String>,
    memory: Option<String>,
}

/// How often the launcher looks at the pod, and how long it waits for its exit code.
struct Timing {
    start: Duration,
    stop: Duration,
    limit: Duration,
}

/// `uwe kuberun <workflow>`: starts `uwe run` in a pod, follows its log and ends with its
/// exit code.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let setup = crate::setup::Setup::load(arguments)?;
    let settings = &setup.executors;
    let kubeconfig = (arguments.kubeconfig.as_deref()).or(settings.kubeconfig.as_deref());
    let kubeconfig = kubeconfig.ok_or_else(|| {
        Failure::usage(
            "uwe: `uwe kuberun` needs -kubeconfig, or kubeconfig in [executor.kubernetes]",
        )
    })?;
    let namespace =
        (arguments.kubernetes_namespace.clone()).or_else(|| settings.kubernetes_namespace.clone());
    let runtime = workflow_graph::runtime().map_err(|error| Failure::setup(error.to_string()))?;
    let cluster = (runtime.block_on(Cluster::connect(kubeconfig, namespace)))
        .map_err(|error| Failure::setup(format!("uwe: {error}")))?;
    let mut random = [0u8; 4];
    let _ = rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty());
    let name = format!(
        "uwe-{:08x}",
        u32::from_le_bytes(random) ^ std::process::id()
    );
    let plan = plan(arguments, &name, cluster.namespace())?;
    let timing = Timing {
        start: Duration::from_secs(1),
        stop: Duration::from_secs(10),
        limit: Duration::from_secs(90),
    };
    let background = arguments.background;
    let code = runtime.block_on(launch(&cluster, &plan, background, &timing));
    match code.map_err(|error| failure(format!("uwe: {error}")))? {
        0 => Ok(()),
        code => Err(Failure::said(code)),
    }
}

/// The plan of a launch: the workflow must be a repository or an absolute path in the
/// cluster, and local config files travel in the ConfigMap.
fn plan(arguments: &Arguments, name: &str, namespace: &str) -> Result<Plan, Failure> {
    let target = (arguments.target.as_deref())
        .ok_or_else(|| Failure::usage("No project name was specified"))?;
    let shown = target.display();
    let repository = crate::repository::Repository::parse(&target.to_string_lossy())?;
    if target.exists() || repository.is_none() && !target.is_absolute() {
        let kind = if target.is_dir() { "project" } else { "script" };
        return Err(Failure::usage(format!(
            "The k8s executor cannot run local {kind}: {shown} -- provide the absolute path of a \
             project available in the Kubernetes cluster or the URL of a project hosted in a Git \
             repository"
        )));
    }
    let image = (arguments.head_image.clone()).ok_or_else(|| {
        Failure::usage("uwe: `uwe kuberun` needs -head-image, an image with uwe on its PATH")
    })?;
    let mut files = BTreeMap::new();
    files.insert("kubeconfig".to_owned(), kubeconfig(namespace));
    let mut command: Vec<String> = ["uwe", "run", &target.to_string_lossy()]
        .map(str::to_owned)
        .to_vec();
    let mut add = |option: &str, value: &str| {
        command.extend([option.to_owned(), value.to_owned()]);
    };
    add("-executor", "kubernetes");
    add("-kubeconfig", &format!("{FILES}/kubeconfig"));
    add("-kubernetes-namespace", namespace);
    let mut ship = |key: String, file: &Path| -> Result<String, Failure> {
        let text = std::fs::read_to_string(file).map_err(|error| {
            Failure::usage(format!("uwe: cannot read {}: {error}", file.display()))
        })?;
        files.insert(key.clone(), text);
        Ok(format!("{FILES}/{key}"))
    };
    let mut shipped = Vec::new();
    if let Some(file) = &arguments.config_file {
        shipped.push(("-uwe-config", ship("uwe.toml".into(), file)?));
    }
    for (index, file) in arguments.configs.iter().enumerate() {
        shipped.push(("-c", ship(format!("config-{index}"), file)?));
    }
    if let Some(file) = &arguments.inputs {
        let extension = file
            .extension()
            .map(|end| end.to_string_lossy().into_owned());
        let key = format!("params.{}", extension.unwrap_or("json".into()));
        shipped.push(("-params-file", ship(key, file)?));
    }
    for (option, value) in shipped {
        add(option, &value);
    }
    let texts = [
        ("-r", arguments.revision.clone()),
        (
            "-lang",
            arguments.language.map(|language| language.to_string()),
        ),
        (
            "-profile",
            arguments.profiles.as_ref().map(|names| names.join(",")),
        ),
        ("-store", arguments.store.clone()),
        ("-default-image", arguments.default_image.clone()),
        (
            "-output-dir",
            (arguments.output_dir.as_ref()).map(|dir| dir.display().to_string()),
        ),
    ];
    for (option, value) in texts {
        if let Some(value) = value {
            add(option, &value);
        }
    }
    for entry in &arguments.targets {
        add("-entry", entry);
    }
    for (flag, on) in [("-latest", arguments.latest), ("-stub", arguments.stub)] {
        if on {
            command.push(flag.to_owned());
        }
    }
    for (name, value) in &arguments.params {
        command.extend([format!("--{name}"), value.clone()]);
    }
    let mut mounts = Vec::new();
    for mount in &arguments.volume_mounts {
        let (claim, path) = mount
            .split_once(':')
            .filter(|(claim, path)| !claim.is_empty() && path.starts_with('/'))
            .ok_or_else(|| {
                Failure::usage(format!(
                    "uwe: -volume-mount takes <claim>:<absolute path>, not `{mount}`"
                ))
            })?;
        mounts.push((claim.to_owned(), path.to_owned()));
    }
    Ok(Plan {
        name: name.to_owned(),
        namespace: namespace.to_owned(),
        image,
        command,
        files,
        mounts,
        cpus: arguments.head_cpus.clone(),
        memory: arguments.head_memory.clone(),
    })
}

/// A kubeconfig for the pod: the API server of the cluster, with the token of the pod's
/// service account, read from its file each time.
fn kubeconfig(namespace: &str) -> String {
    let account = "/var/run/secrets/kubernetes.io/serviceaccount";
    let config = json!({
        "apiVersion": "v1",
        "kind": "Config",
        "clusters": [{ "name": "cluster", "cluster": {
            "server": "https://kubernetes.default.svc",
            "certificate-authority": format!("{account}/ca.crt"),
        }}],
        "users": [{ "name": "driver", "user": { "tokenFile": format!("{account}/token") } }],
        "contexts": [{ "name": "driver", "context": {
            "cluster": "cluster", "user": "driver", "namespace": namespace,
        }}],
        "current-context": "driver",
    });
    // JSON is YAML, which kubeconfig files are.
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// The service account of the pod, with a Role of what `-executor kubernetes` needs in its
/// namespace: its Jobs, their pods and logs, and the events that say why a pod waits.
fn access(plan: &Plan) -> [Json; 3] {
    let name = &plan.name;
    let account = json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": { "name": name, "labels": labels(plan) },
    });
    let role = json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
        "metadata": { "name": name, "labels": labels(plan) },
        "rules": [
            { "apiGroups": ["batch"], "resources": ["jobs"],
              "verbs": ["create", "get", "list", "patch", "delete"] },
            { "apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list"] },
            { "apiGroups": [""], "resources": ["pods/log"], "verbs": ["get"] },
            { "apiGroups": [""], "resources": ["events"], "verbs": ["list"] },
        ],
    });
    let binding = json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": { "name": name, "labels": labels(plan) },
        "subjects": [{ "kind": "ServiceAccount", "name": name, "namespace": plan.namespace }],
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": name },
    });
    [account, role, binding]
}

fn labels(plan: &Plan) -> Json {
    json!({ "app": "uwe", "runName": plan.name })
}

/// The ConfigMap with the config files of the launch.
fn config_map(plan: &Plan) -> Json {
    json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": { "name": plan.name, "labels": labels(plan) },
        "data": plan.files,
    })
}

/// The pod of the driver: not privileged, as a user without root and without its own
/// capabilities, with a read-only root file system.
fn pod(plan: &Plan) -> Json {
    let work = (plan.mounts.first()).map_or(WORK, |(_, path)| path.as_str());
    let mut volumes = vec![json!({ "name": "files", "configMap": { "name": plan.name } })];
    let mut mounts = vec![json!({ "name": "files", "mountPath": FILES, "readOnly": true })];
    if plan.mounts.is_empty() {
        volumes.push(json!({ "name": "work", "emptyDir": {} }));
        mounts.push(json!({ "name": "work", "mountPath": WORK }));
    }
    for (index, (claim, path)) in plan.mounts.iter().enumerate() {
        let name = format!("claim-{index}");
        volumes.push(json!({ "name": name, "persistentVolumeClaim": { "claimName": claim } }));
        mounts.push(json!({ "name": name, "mountPath": path }));
    }
    let mut requests = serde_json::Map::new();
    for (key, value) in [("cpu", &plan.cpus), ("memory", &plan.memory)] {
        if let Some(value) = value {
            requests.insert(key.into(), json!(value));
        }
    }
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": { "name": plan.name, "labels": labels(plan) },
        "spec": {
            "serviceAccountName": plan.name,
            "restartPolicy": "Never",
            "securityContext": {
                "runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
            "containers": [{
                "name": "uwe",
                "image": plan.image,
                "command": plan.command,
                "workingDir": work,
                "env": [
                    { "name": "HOME", "value": work },
                    { "name": "XDG_CACHE_HOME", "value": format!("{work}/.cache") },
                ],
                "resources": { "requests": requests },
                "securityContext": {
                    "privileged": false,
                    "allowPrivilegeEscalation": false,
                    "readOnlyRootFilesystem": true,
                    "capabilities": { "drop": ["ALL"] },
                },
                "volumeMounts": mounts,
            }],
            "volumes": volumes,
        },
    })
}

/// Makes the service account, its Role, the ConfigMap and the pod, which owns the others so
/// that they go with it; then follows the pod and gives its exit code.
async fn launch(
    cluster: &Cluster,
    plan: &Plan,
    background: bool,
    timing: &Timing,
) -> Result<u8, String> {
    let client = cluster.client();
    let namespace = plan.namespace.as_str();
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let made = make(&client, plan).await;
    let uid = match made {
        Ok(uid) => uid,
        Err(error) => {
            remove(&client, plan).await;
            return Err(error);
        }
    };
    own(&client, plan, &uid).await?;
    print!("Pod submitted: {} .. waiting to start", plan.name);
    loop {
        let pod = pods
            .get(&plan.name)
            .await
            .map_err(|error| error.to_string())?;
        if phase(&pod) != "Pending" {
            break;
        }
        tokio::time::sleep(timing.start).await;
    }
    println!("\nPod started: {}", plan.name);
    if background {
        return Ok(0);
    }
    follow(&pods, &plan.name).await;
    let code = exit_code(&pods, plan, timing).await;
    if code == 0 {
        let deleted = pods.delete(&plan.name, &DeleteParams::default()).await;
        if deleted.is_err() {
            tracing::warn!("Unable to delete pod: {}", plan.name);
        }
    } else {
        eprintln!(
            "uwe: the pod {} and its service account stay in namespace {namespace} for a look",
            plan.name
        );
    }
    Ok(code)
}

/// The kinds of the objects that the pod owns, in the order they are made.
fn owned() -> [ApiResource; 4] {
    [
        ApiResource::erase::<ServiceAccount>(&()),
        ApiResource::erase::<Role>(&()),
        ApiResource::erase::<RoleBinding>(&()),
        ApiResource::erase::<ConfigMap>(&()),
    ]
}

/// Makes the objects of the launch; the uid of the pod.
async fn make(client: &Client, plan: &Plan) -> Result<String, String> {
    let post = PostParams::default();
    let [account, role, binding] = access(plan);
    let objects = [account, role, binding, config_map(plan)];
    for (resource, object) in owned().iter().zip(objects) {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &plan.namespace, resource);
        let object = serde_json::from_value(object).map_err(|error| error.to_string())?;
        let made = api.create(&post, &object).await;
        made.map_err(|error| format!("cannot make the {}: {error}", resource.kind))?;
    }
    let pods: Api<Pod> = Api::namespaced(client.clone(), &plan.namespace);
    let pod = serde_json::from_value(pod(plan)).map_err(|error| error.to_string())?;
    let made = pods.create(&post, &pod).await;
    let made = made.map_err(|error| format!("cannot make the pod: {error}"))?;
    made.metadata
        .uid
        .ok_or_else(|| "the pod has no uid".to_owned())
}

/// Makes the pod the owner of the other objects, so that deleting it deletes them.
async fn own(client: &Client, plan: &Plan, uid: &str) -> Result<(), String> {
    let owner = json!({ "metadata": { "ownerReferences": [{
        "apiVersion": "v1", "kind": "Pod", "name": plan.name, "uid": uid,
    }]}});
    let (parameters, patch) = (PatchParams::default(), Patch::Merge(&owner));
    for resource in owned() {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &plan.namespace, &resource);
        let patched = api.patch(&plan.name, &parameters, &patch).await;
        patched.map_err(|error| format!("cannot let the pod own its objects: {error}"))?;
    }
    Ok(())
}

/// Deletes what a failed launch made, as far as it got.
async fn remove(client: &Client, plan: &Plan) {
    for resource in owned() {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &plan.namespace, &resource);
        let _ = api.delete(&plan.name, &DeleteParams::default()).await;
    }
}

/// Prints the log of the driver line by line until it ends.
async fn follow(pods: &Api<Pod>, name: &str) {
    use futures_util::AsyncBufReadExt;
    use futures_util::StreamExt;
    let parameters = LogParams {
        follow: true,
        container: Some("uwe".into()),
        ..LogParams::default()
    };
    let Ok(stream) = pods.log_stream(name, &parameters).await else {
        tracing::warn!("Unable to follow the log of pod {name}");
        return;
    };
    let mut lines = stream.lines();
    while let Some(Ok(line)) = lines.next().await {
        println!("{line}");
    }
}

/// The exit code of the driver once it stopped, else 127 after `timing.limit`.
async fn exit_code(pods: &Api<Pod>, plan: &Plan, timing: &Timing) -> u8 {
    let started = tokio::time::Instant::now();
    let mut said = false;
    loop {
        let found = pods.get(&plan.name).await;
        let state = found.as_ref().map(phase).unwrap_or("unknown").to_owned();
        if let Ok(pod) = &found
            && let Some(code) = terminated(pod)
        {
            return u8::try_from(code).unwrap_or(1);
        }
        if started.elapsed() >= timing.limit {
            tracing::warn!(
                "Unable to fetch pod exit status -- pod={} state={state}",
                plan.name
            );
            return 127;
        }
        if state == "Running" && !said {
            println!(
                "Pod running: {} ... waiting for pod to stop running",
                plan.name
            );
            said = true;
        }
        tokio::time::sleep(timing.stop).await;
    }
}

fn phase(pod: &Pod) -> &str {
    let status = pod.status.as_ref();
    status
        .and_then(|status| status.phase.as_deref())
        .unwrap_or("Pending")
}

/// The exit code of the container of uwe, once it has ended.
fn terminated(pod: &Pod) -> Option<i32> {
    let statuses = pod.status.as_ref()?.container_statuses.as_ref()?;
    let status = statuses.iter().find(|status| status.name == "uwe")?;
    Some(status.state.as_ref()?.terminated.as_ref()?.exit_code)
}
