//! Loads executor settings, command defaults and storage configuration.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use config_file::{ConfigError, ConfigFile, Secret, Table};
use content_store::Store;
use slurm_executor::PullResources;
use storage::{Storage, StoreLocation};
use task_runner::NetworkRule;

use crate::Failure;
use crate::arguments::Arguments;
use crate::insight::telemetry::Telemetry;

/// The sections of the config file. Another name is an error, so that a typing mistake does
/// not go unnoticed. `executor` holds the settings of executors.
const SECTIONS: &[&str] = &[
    "store",
    "storage",
    "executor",
    "telemetry",
    "registry",
    "network",
    "cache",
    "report",
    "conda",
    "cwl",
    "page",
    "retry",
    "secrets",
    "workflows",
    "references",
];

/// The executors that `-executor` and `default` of `[executor]` may name.
pub(crate) const EXECUTORS: &[&str] = &[
    "simulator",
    "local",
    "tes",
    "slurm",
    "kubernetes",
    "aws-batch",
    "google-batch",
    "azure-batch",
    "nomad",
    "htcondor",
    "pbs",
    "flyte",
    "snowflake",
    "jarvice",
    "wr",
];

/// What the config files set up for a command; the default is what no config file sets up.
#[derive(Default)]
pub struct Setup {
    pub references: task_runner::References,
    pub hosted: bool,
    /// What workflow code may reach while uwe interprets it; `None` reaches what the process
    /// can, as on the command line.
    pub isolation: Option<crate::hosted::Isolation>,
    pub argo_policy: argo_runtime::Policy,
    pub argo_locks: argo_runtime::SharedLocks,
    pub cluster: Option<kubernetes_executor::Cluster>,
    /// The storage providers, one client each for the whole process.
    pub storage: Storage,
    pub registry: executor::credentials::Registry,
    /// The settings of the executors.
    pub executors: Executors,
    /// The CPUs and memory that local tasks take their shares from. A host gives the same pool
    /// to all its runs, so that they share it; `None` gives the run its own pool.
    pub local_pool: Option<executor::Pool>,
    /// Where the traces of runs go, from `[telemetry]`.
    pub telemetry: Option<Telemetry>,
    /// Which tasks get the network, from `[network]`.
    pub network: NetworkRule,
    pub conda: Option<crate::conda::Config>,
    pub secrets: std::collections::BTreeMap<String, Secret>,
    /// The values of the run for the variables that its jobs get, like the `envvars` of
    /// Snakemake; the process environment unless the run gives its own.
    pub variables: config_file::SecretValues,
    /// `[cwl] registry`: where images that CWL tools build go for remote executors.
    pub cwl_registry: Option<String>,
}

/// The settings of executors from the `[executor.<name>]` sections. Options on the command line
/// and environment variables win over them.
#[derive(Clone, Debug, Default)]
pub struct Executors {
    pub local_cpus: Option<u32>,
    pub local_memory: Option<u64>,
    pub kubernetes_in_cluster: bool,
    pub kubernetes_references: Vec<kubernetes_executor::SharedVolume>,
    /// `[executor.local] engine`: the container engine, as `UWE_ENGINE` names it.
    pub engine: Option<String>,
    /// `[executor.tes] server`, like `-tes-server`.
    pub tes_server: Option<String>,
    /// `[executor.tes] token`, like `UWE_TES_TOKEN`.
    pub tes_token: Option<Secret>,
    pub tes: tes_executor::Settings,
    /// `[executor.tes.sites.<name>]`: more TES servers, each near some data.
    pub tes_sites: Vec<TesSite>,
    /// `[executor.slurm] container`, like `-slurm-container`.
    pub slurm_container: Option<String>,
    /// `[executor.slurm] options`, a list of options of `sbatch`, like `-slurm-options`.
    pub slurm_options: Option<Vec<String>>,
    /// `[executor.slurm.pull]`: `cpus`, `memory_mb` and `minutes` of the job that pulls an
    /// image into a SIF file.
    pub slurm_pull: PullResources,
    /// `[executor.slurm] query_seconds`: how long one call of `squeue`, `sacct` or `scancel`
    /// may take, for a busy controller.
    pub slurm_query: Option<u64>,
    /// `[executor.slurm] staging` and the settings of its staging root.
    pub slurm_staging: crate::staging::Settings,
    /// `[executor.slurm] helper_path`, or `helper_url` with `helper_sha256`: where the jobs get
    /// the static uwe binary for object-store staging.
    pub slurm_helper: Option<slurm_executor::Helper>,
    /// Without `slurm_helper`: `[executor.slurm] helper_file`, or else the running binary,
    /// which uwe uploads to the staging root for the jobs.
    pub slurm_helper_file: Option<PathBuf>,
    /// `[executor.slurm] node_folder`: the folder of the nodes for the files of a job.
    pub slurm_node_folder: Option<String>,
    /// `[executor.slurm] memory_per_cpu_gb`: for a cluster that gives each CPU a fixed memory,
    /// the GB per CPU; the jobs then ask for CPUs instead of memory.
    pub slurm_cpu_memory: Option<f64>,
    /// `[executor.slurm] staging_secret_file`: the storage providers of the staging root for
    /// `staging_access = "credentials"`, in a file that only its owner may read.
    pub slurm_secret_file: Option<PathBuf>,
    /// `[executor.kubernetes] kubeconfig`, like `-kubeconfig`.
    pub kubeconfig: Option<PathBuf>,
    /// `[executor.kubernetes] namespace`, like `-kubernetes-namespace`.
    pub kubernetes_namespace: Option<String>,
    /// `[executor.kubernetes] staging` and the settings of its staging root.
    pub kubernetes_staging: crate::staging::Settings,
    /// `[executor.kubernetes] unschedulable_minutes`: how long a pod may find no node.
    pub kubernetes_unschedulable: Option<u64>,
    /// `[executor.kubernetes] start_grace_seconds`: how long a Job may have no pod, or a pod
    /// that cannot start, before uwe asks the cluster why.
    pub kubernetes_start_grace: Option<u64>,
    /// `[executor.kubernetes] helper_image` and `helper_mount`: the image that holds only the
    /// static uwe binary, for object-store staging; or else `helper_download_image`, which
    /// downloads the binary.
    pub kubernetes_helper: Option<kubernetes_executor::Helper>,
    /// Without `helper_image`: `[executor.kubernetes] helper_file`, or else the running binary,
    /// which uwe uploads to the staging root for the pods.
    pub kubernetes_helper_file: Option<PathBuf>,
    /// `[executor.kubernetes.shared_volume]`: a claim or an NFS export that holds the store and
    /// scratch folders, instead of `hostPath` volumes.
    pub kubernetes_volumes: kubernetes_executor::Volumes,
    /// `[executor.kubernetes] kubedal_data_node` and the other kubedal settings, for
    /// `staging = "kubedal"`.
    pub kubernetes_kubedal: Option<kubernetes_executor::Kubedal>,
    /// `[executor.kubernetes] context`: the context of the kubeconfig, else its current one.
    pub kubernetes_context: Option<String>,
    /// `[executor.kubernetes] service_account` and `pod`: the service account and the pod
    /// options of every Job, as Nextflow's `pod` directive takes them.
    pub kubernetes_pod: kubernetes_executor::PodOptions,
    /// `[executor.kubernetes.cws]`: the Common Workflow Scheduler that places the pods.
    pub kubernetes_cws: Option<kubernetes_executor::cws::Settings>,
    /// `[executor.aws-batch]`: the jobs, their staging root and their helper.
    pub aws_batch: crate::aws_batch::Config,
    /// `[executor.google-batch]`: the jobs, their staging root and their helper.
    pub google_batch: crate::google_batch::Config,
    /// `[executor.azure-batch]`: the tasks, their pools, staging root and helper.
    pub azure_batch: crate::azure_batch::Config,
    /// `[executor.nomad]`: the agent, the jobs, their staging and helper.
    pub nomad: crate::nomad::Config,
    /// `[executor.htcondor]` and `[executor.pbs]`: the jobs, their staging and helper.
    pub htcondor: crate::grid::Config,
    pub pbs: crate::grid::Config,
    /// `[executor.flyte]`: the control plane, the runs, their staging and helper.
    pub flyte: crate::flyte::Config,
    /// `[executor.snowflake]`: the account, the job services, their staging and helper.
    pub snowflake: crate::snowflake::Config,
    /// `[executor.jarvice]`: the API, the jobs, their vault or staging root and helper.
    pub jarvice: crate::jarvice::Config,
    /// `[executor.wr]`: the wr manager, the jobs, their staging and helper.
    pub wr: crate::wr::Config,
}

/// A site of `[executor.tes.sites.<name>]`: a TES server, and the address prefixes or folders
/// of the data near it, which decide the tasks that run there.
#[derive(Clone, Debug)]
pub struct TesSite {
    pub name: String,
    pub server: String,
    pub token: Option<Secret>,
    pub near: Vec<String>,
    pub settings: tes_executor::Settings,
}

impl Executors {
    /// The settings of the `[executor]` section of a config file.
    fn read(config: &ConfigFile) -> Result<Executors, ConfigError> {
        let mut settings = Executors::default();
        let Some(section) = config.section("executor")? else {
            return Ok(settings);
        };
        let keys = [
            "default",
            "task_minutes",
            "local",
            "tes",
            "slurm",
            "kubernetes",
            "aws-batch",
            "google-batch",
            "azure-batch",
            "nomad",
            "htcondor",
            "pbs",
            "flyte",
            "snowflake",
            "jarvice",
            "wr",
        ];
        section.only(&keys)?;
        if let Some(batch) = section.table("aws-batch")? {
            settings.aws_batch = crate::aws_batch::Config::read(&batch)?;
        }
        if let Some(batch) = section.table("google-batch")? {
            settings.google_batch = crate::google_batch::Config::read(&batch)?;
        }
        if let Some(batch) = section.table("azure-batch")? {
            settings.azure_batch = crate::azure_batch::Config::read(&batch)?;
        }
        if let Some(nomad) = section.table("nomad")? {
            settings.nomad = crate::nomad::Config::read(&nomad)?;
        }
        if let Some(htcondor) = section.table("htcondor")? {
            settings.htcondor = crate::grid::Config::read(&htcondor, false)?;
        }
        if let Some(pbs) = section.table("pbs")? {
            settings.pbs = crate::grid::Config::read(&pbs, true)?;
        }
        if let Some(flyte) = section.table("flyte")? {
            settings.flyte = crate::flyte::Config::read(&flyte)?;
        }
        if let Some(snowflake) = section.table("snowflake")? {
            settings.snowflake = crate::snowflake::Config::read(&snowflake)?;
        }
        if let Some(jarvice) = section.table("jarvice")? {
            settings.jarvice = crate::jarvice::Config::read(&jarvice)?;
        }
        if let Some(wr) = section.table("wr")? {
            settings.wr = crate::wr::Config::read(&wr)?;
        }
        if let Some(local) = section.table("local")? {
            local.only(&["engine", "cpus", "memory_bytes"])?;
            settings.engine = local.text("engine")?;
            settings.local_cpus = local
                .integer("cpus")?
                .map(|value| {
                    u32::try_from(value)
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| local.invalid("cpus", "must be a positive CPU capacity"))
                })
                .transpose()?;
            settings.local_memory = local
                .integer("memory_bytes")?
                .map(|value| {
                    u64::try_from(value)
                        .ok()
                        .filter(|value| *value >= 6 << 20)
                        .ok_or_else(|| local.invalid("memory_bytes", "must be at least 6 MiB"))
                })
                .transpose()?;
        }
        if let Some(tes) = section.table("tes")? {
            let mut keys = vec!["server", "token", "sites"];
            keys.extend(tes_executor::Settings::KEYS);
            tes.only(&keys)?;
            settings.tes_server = tes.text("server")?;
            settings.tes_token = tes.secret("token")?;
            settings.tes = tes_executor::Settings::read(&tes)?;
            for site in (tes.table("sites")?.map(|sites| sites.tables()))
                .transpose()?
                .into_iter()
                .flatten()
            {
                let mut keys = vec!["server", "token", "near"];
                keys.extend(tes_executor::Settings::KEYS);
                site.only(&keys)?;
                let server = site.text("server")?;
                let server = server
                    .ok_or_else(|| site.missing("server", "the address of its TES server"))?;
                settings.tes_sites.push(TesSite {
                    // The table is named by its whole path, like `executor.tes.sites.hamburg`.
                    name: (site.name().strip_prefix("executor.tes.sites."))
                        .unwrap_or(site.name())
                        .to_owned(),
                    server,
                    token: site.secret("token")?,
                    near: site.texts("near")?.unwrap_or_default(),
                    settings: tes_executor::Settings::read(&site)?,
                });
            }
        }
        if let Some(slurm) = section.table("slurm")? {
            let mut keys = vec![
                "container",
                "options",
                "pull",
                "node_folder",
                "query_seconds",
                "memory_per_cpu_gb",
            ];
            keys.push("staging_secret_file");
            keys.extend(["helper_path", "helper_url", "helper_sha256", "helper_file"]);
            keys.extend(crate::staging::Settings::KEYS);
            slurm.only(&keys)?;
            settings.slurm_staging = crate::staging::Settings::read(&slurm, &["object-store"])?;
            settings.slurm_helper = helper(&slurm)?;
            settings.slurm_node_folder = slurm.text("node_folder")?;
            settings.slurm_secret_file = slurm.text("staging_secret_file")?.map(PathBuf::from);
            if settings.slurm_staging.credentials != settings.slurm_secret_file.is_some() {
                let message = "names the file of the storage providers for staging_access = \
                               \"credentials\"";
                return Err(slurm.invalid("staging_secret_file", message));
            }
            settings.slurm_helper_file = helper_file(&slurm, settings.slurm_helper.is_some())?;
            if settings.slurm_staging.mode != crate::staging::Mode::ObjectStore {
                settings.slurm_helper_file = None;
            }
            settings.slurm_container = slurm.text("container")?;
            if let Some(gb) = slurm.number("memory_per_cpu_gb")? {
                if !(gb > 0.0 && gb.is_finite()) {
                    return Err(slurm.invalid("memory_per_cpu_gb", "must be above 0"));
                }
                settings.slurm_cpu_memory = Some(gb);
            }
            settings.slurm_options = slurm.texts("options")?;
            if let Some(pull) = slurm.table("pull")? {
                settings.slurm_pull = pull_resources(&pull)?;
            }
            if let Some(seconds) = slurm.integer("query_seconds")? {
                if !(1..=3600).contains(&seconds) {
                    return Err(slurm.invalid("query_seconds", "must be between 1 and 3600"));
                }
                settings.slurm_query = Some(seconds as u64);
            }
        }
        if let Some(kubernetes) = section.table("kubernetes")? {
            let mut keys = vec![
                "kubeconfig",
                "namespace",
                "helper_image",
                "helper_mount",
                "in_cluster",
                "references",
            ];
            keys.extend(["staging_secret", "helper_file", "helper_download_image"]);
            keys.extend(["shared_volume", "kubedal_driver", "kubedal_data_node"]);
            keys.extend([
                "kubedal_namespace",
                "kubedal_upload_minutes",
                "unschedulable_minutes",
                "start_grace_seconds",
                "context",
                "service_account",
                "pod",
                "cws",
            ]);
            keys.extend(crate::staging::Settings::KEYS);
            kubernetes.only(&keys)?;
            settings.kubeconfig = kubernetes.text("kubeconfig")?.map(PathBuf::from);
            settings.kubernetes_namespace = kubernetes.text("namespace")?;
            settings.kubernetes_in_cluster = kubernetes.flag("in_cluster")?.unwrap_or(false);
            settings.kubernetes_context = kubernetes.text("context")?;
            settings.kubernetes_pod.service_account = kubernetes.text("service_account")?;
            if let Some(pod) = kubernetes.json("pod")? {
                let pods = &mut settings.kubernetes_pod;
                pods.add(&pod)
                    .map_err(|message| kubernetes.invalid("pod", message))?;
            }
            if let Some(cws) = kubernetes.table("cws")? {
                cws.only(&["address", "strategy", "cost_function", "scheduler_name"])?;
                let address = (cws.text("address")?)
                    .ok_or_else(|| cws.missing("address", "the address of the scheduler"))?;
                if !address.starts_with("http://") && !address.starts_with("https://") {
                    return Err(cws.invalid("address", "must be an http:// or https:// address"));
                }
                let mut scheduler = kubernetes_executor::cws::Settings::new(&address);
                scheduler.strategy = cws.text("strategy")?.unwrap_or(scheduler.strategy);
                scheduler.cost_function = cws.text("cost_function")?;
                let name = cws.text("scheduler_name")?;
                scheduler.scheduler_name = name.unwrap_or(scheduler.scheduler_name);
                settings.kubernetes_cws = Some(scheduler);
            }
            if let Some(references) = kubernetes.table("references")? {
                for reference in references.tables()? {
                    reference.only(&["path", "claim", "nfs_server", "nfs_path"])?;
                    let path = reference
                        .text("path")?
                        .ok_or_else(|| reference.missing("path", "the installed reference root"))?;
                    let source = match (
                        reference.text("claim")?,
                        reference.text("nfs_server")?,
                        reference.text("nfs_path")?,
                    ) {
                        (Some(claim), None, None) => {
                            serde_json::json!({"persistentVolumeClaim": {"claimName": claim}})
                        }
                        (None, Some(server), Some(path)) => {
                            serde_json::json!({"nfs": {"server": server, "path": path}})
                        }
                        _ => {
                            return Err(reference.invalid(
                                "claim",
                                "supply either claim or both nfs_server and nfs_path",
                            ));
                        }
                    };
                    let path = PathBuf::from(path);
                    if !path.is_absolute() {
                        return Err(reference.invalid("path", "must be absolute"));
                    }
                    settings
                        .kubernetes_references
                        .push(kubernetes_executor::SharedVolume { path, source });
                }
            }
            if let Some(minutes) = kubernetes.integer("unschedulable_minutes")? {
                if !(1..=10_080).contains(&minutes) {
                    let message = "must be between 1 and 10080";
                    return Err(kubernetes.invalid("unschedulable_minutes", message));
                }
                settings.kubernetes_unschedulable = Some(minutes as u64);
            }
            if let Some(seconds) = kubernetes.integer("start_grace_seconds")? {
                if !(10..=3600).contains(&seconds) {
                    let message = "must be between 10 and 3600";
                    return Err(kubernetes.invalid("start_grace_seconds", message));
                }
                settings.kubernetes_start_grace = Some(seconds as u64);
            }
            let modes = ["object-store", "kubedal"];
            settings.kubernetes_staging = crate::staging::Settings::read(&kubernetes, &modes)?;
            let image_volume = match kubernetes.text("helper_mount")?.as_deref() {
                None | Some("image-volume") => true,
                Some("init-container") => false,
                Some(_) => {
                    let message = "must be image-volume or init-container";
                    return Err(kubernetes.invalid("helper_mount", message));
                }
            };
            let secret = kubernetes.text("staging_secret")?;
            if settings.kubernetes_staging.credentials != secret.is_some() {
                let message = "names the Secret of the storage providers for staging_access = \
                               \"credentials\"";
                return Err(kubernetes.invalid("staging_secret", message));
            }
            let image = kubernetes.text("helper_image")?;
            let download = kubernetes.text("helper_download_image")?;
            if image.is_some() && download.is_some() {
                return Err(kubernetes.invalid("helper_download_image", "needs no helper_image"));
            }
            if image.is_none() && kubernetes.text("helper_mount")?.is_some() {
                return Err(kubernetes.invalid("helper_mount", "needs helper_image"));
            }
            let object_store =
                settings.kubernetes_staging.mode == crate::staging::Mode::ObjectStore;
            settings.kubernetes_helper_file = helper_file(&kubernetes, image.is_some())?;
            if settings.kubernetes_staging.mode == crate::staging::Mode::SharedPath {
                settings.kubernetes_helper_file = None;
            }
            // Without helper_image the pods download the binary with Alpine.
            let image = match (image, object_store) {
                (Some(image), _) => Some(image),
                (None, true) => {
                    Some(download.unwrap_or_else(|| kubernetes_executor::DOWNLOAD_IMAGE.into()))
                }
                (None, false) => None,
            };
            settings.kubernetes_helper = image.map(|image| kubernetes_executor::Helper {
                image,
                image_volume,
                secret,
                binary: None,
            });
            settings.kubernetes_kubedal = kubedal(&kubernetes)?;
            if (settings.kubernetes_staging.mode == crate::staging::Mode::Kubedal)
                != settings.kubernetes_kubedal.is_some()
            {
                let message = "names the DataNode of the staging root for staging = \"kubedal\"";
                return Err(kubernetes.invalid("kubedal_data_node", message));
            }
            if let Some(shared) = kubernetes.table("shared_volume")? {
                if settings.kubernetes_staging.mode != crate::staging::Mode::SharedPath {
                    let message = "needs staging = \"shared-path\"";
                    return Err(kubernetes.invalid("shared_volume", message));
                }
                settings.kubernetes_volumes = shared_volume(&shared)?;
            }
        }
        Ok(settings)
    }
}

/// The kubedal setup of `[executor.kubernetes]`: the DataNode whose folder is the staging root,
/// its namespace, the name of the driver, and how long kubedal may upload after a pod.
fn kubedal(kubernetes: &Table<'_>) -> Result<Option<kubernetes_executor::Kubedal>, ConfigError> {
    let Some(data_node) = kubernetes.text("kubedal_data_node")? else {
        return Ok(None);
    };
    let minutes = match kubernetes.integer("kubedal_upload_minutes")? {
        None => 60,
        Some(minutes) if minutes > 0 => minutes as u64,
        Some(_) => return Err(kubernetes.invalid("kubedal_upload_minutes", "must be above 0")),
    };
    Ok(Some(kubernetes_executor::Kubedal {
        driver: (kubernetes.text("kubedal_driver")?)
            .unwrap_or_else(|| "kubedal.arunaengine.org".into()),
        data_node,
        namespace: kubernetes.text("kubedal_namespace")?,
        upload: std::time::Duration::from_secs(minutes * 60),
        helper: None,
    }))
}

/// The volume of `[executor.kubernetes.shared_volume]`: a `claim` or an `nfs_server` with its
/// `nfs_path`, which pods mount at `path`, where this machine sees it too.
fn shared_volume(shared: &Table<'_>) -> Result<kubernetes_executor::Volumes, ConfigError> {
    shared.only(&["claim", "nfs_server", "nfs_path", "path"])?;
    let path = shared.text("path")?.ok_or_else(|| {
        shared.missing(
            "path",
            "the folder where this machine and the pods see the volume",
        )
    })?;
    if !path.starts_with('/') {
        return Err(shared.invalid("path", "must be an absolute path"));
    }
    let claim = shared.text("claim")?;
    let nfs = (shared.text("nfs_server")?, shared.text("nfs_path")?);
    let source = match (claim, nfs) {
        (Some(claim), (None, None)) => {
            serde_json::json!({"persistentVolumeClaim": {"claimName": claim}})
        }
        (None, (Some(server), Some(path))) => {
            serde_json::json!({"nfs": {"server": server, "path": path}})
        }
        _ => {
            let message = "needs either claim, or nfs_server with nfs_path";
            return Err(shared.invalid("claim", message));
        }
    };
    Ok(kubernetes_executor::Volumes::Under {
        root: PathBuf::from(path),
        source,
    })
}

/// Where the SLURM jobs get the uwe binary: `helper_path` on the nodes, or `helper_url` with
/// its `helper_sha256`.
pub(crate) fn helper(slurm: &Table<'_>) -> Result<Option<slurm_executor::Helper>, ConfigError> {
    let (path, url) = (slurm.text("helper_path")?, slurm.text("helper_url")?);
    let sha256 = slurm.text("helper_sha256")?;
    match (path, url, sha256) {
        (None, None, None) => Ok(None),
        (Some(path), None, None) => Ok(Some(slurm_executor::Helper::Path(path))),
        (None, Some(url), Some(sha256))
            if sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            let sha256 = sha256.to_ascii_lowercase();
            Ok(Some(slurm_executor::Helper::Download { url, sha256 }))
        }
        (None, Some(_), _) => Err(slurm.invalid(
            "helper_sha256",
            "must be the SHA-256 of the file at helper_url, as 64 hex digits",
        )),
        _ => Err(slurm.invalid(
            "helper_path",
            "takes no helper_url; name the binary on the nodes or the address to download",
        )),
    }
}

/// The file of `helper_file`, or else the running binary, unless the table names where the jobs
/// get the binary already.
pub(crate) fn helper_file(table: &Table<'_>, named: bool) -> Result<Option<PathBuf>, ConfigError> {
    match (table.text("helper_file")?, named) {
        (Some(_), true) => {
            let message = "takes no other helper setting; name one place for the binary";
            Err(table.invalid("helper_file", message))
        }
        (Some(file), false) => Ok(Some(PathBuf::from(file))),
        (None, true) => Ok(None),
        (None, false) => std::env::current_exe()
            .map(Some)
            .map_err(|error| table.invalid("helper_file", format!("is needed: {error}"))),
    }
}

/// The resources of the job that pulls an image, from `[executor.slurm.pull]`; what it leaves
/// out keeps its default.
fn pull_resources(pull: &Table<'_>) -> Result<PullResources, ConfigError> {
    pull.only(&["cpus", "memory_mb", "minutes"])?;
    let positive = |key: &str| -> Result<Option<u64>, ConfigError> {
        match pull.integer(key)? {
            None => Ok(None),
            Some(value) if value > 0 => Ok(Some(value as u64)),
            Some(value) => Err(pull.invalid(key, format!("is {value}; use a number above 0"))),
        }
    };
    let mut resources = PullResources::default();
    if let Some(cpus) = positive("cpus")? {
        resources.cpus = u32::try_from(cpus).unwrap_or(u32::MAX);
    }
    if let Some(memory_mb) = positive("memory_mb")? {
        resources.memory_mb = memory_mb;
    }
    if let Some(minutes) = positive("minutes")? {
        resources.time_seconds = minutes.saturating_mul(60);
    }
    Ok(resources)
}

/// Which tasks get the network, from `[network]`: `default = "none"` runs the tasks whose
/// workflow does not say without network (`"allowed"` is the default), and `allowed_steps`
/// names the steps that get it in any case.
fn network(config: &ConfigFile) -> Result<NetworkRule, ConfigError> {
    let Some(section) = config.section("network")? else {
        return Ok(NetworkRule::default());
    };
    section.only(&["default", "allowed_steps", "proxy"])?;
    let none_by_default = match section.text("default")?.as_deref() {
        None | Some("allowed") => false,
        Some("none") => true,
        Some(other) => {
            let message = format!("is `{other}`; use \"none\" or \"allowed\"");
            return Err(section.invalid("default", message));
        }
    };
    Ok(NetworkRule {
        none_by_default,
        allowed_steps: section.texts("allowed_steps")?.unwrap_or_default(),
    })
}

/// `[cwl] registry`: an image repository like `host/team/cwl`, without tag or digest.
fn cwl_registry(config: &ConfigFile) -> Result<Option<String>, ConfigError> {
    let Some(section) = config.section("cwl")? else {
        return Ok(None);
    };
    section.only(&["registry"])?;
    let Some(registry) = section.text("registry")? else {
        return Ok(None);
    };
    let parsed = executor::registry::Reference::parse(&registry);
    let allowed =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"/:._-".contains(&byte);
    if parsed.name != registry
        || parsed.digest.is_some()
        || !registry.bytes().all(allowed)
        || parsed.repository.split('/').any(str::is_empty)
    {
        let message = "must be an image repository without a scheme, tag, digest or credentials";
        return Err(section.invalid("registry", message));
    }
    Ok(Some(registry))
}

/// Config defaults applied before command options, cache and network setup.
#[derive(Debug, Default, PartialEq)]
pub struct Defaults {
    pub executor: Option<String>,
    pub cache_folder: Option<PathBuf>,
    pub proxy: Option<String>,
    pub report: Option<PathBuf>,
    pub machine_retries: Option<u32>,
    pub retry_delay_ms: Option<u64>,
    /// `[executor] task_minutes`, in seconds: the time limit of tasks that set none.
    pub task_seconds: Option<u64>,
}

impl Defaults {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Defaults, Failure> {
        let failed = |error: ConfigError| Failure::setup(format!("uwe: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        Defaults::read(&config).map_err(failed)
    }

    fn read(config: &ConfigFile) -> Result<Defaults, ConfigError> {
        let mut defaults = Defaults::default();
        if let Some(executor) = config.section("executor")? {
            defaults.executor = executor.text("default")?;
            defaults.task_seconds = match executor.integer("task_minutes")? {
                None => None,
                Some(minutes) if minutes > 0 => Some((minutes as u64).saturating_mul(60)),
                Some(_) => return Err(executor.invalid("task_minutes", "use a number above 0")),
            };
            if let Some(name) = &defaults.executor
                && !EXECUTORS.contains(&name.as_str())
            {
                let message = format!("is `{name}`; use one of {}", EXECUTORS.join(", "));
                return Err(executor.invalid("default", message));
            }
        }
        if let Some(cache) = config.section("cache")? {
            cache.only(&["folder"])?;
            defaults.cache_folder = cache.text("folder")?.map(PathBuf::from);
            if let Some(folder) = &defaults.cache_folder
                && !folder.is_absolute()
            {
                let message = format!("is `{}`; use an absolute folder", folder.display());
                return Err(cache.invalid("folder", message));
            }
        }
        if let Some(network) = config.section("network")? {
            defaults.proxy = network.text("proxy")?;
        }
        if let Some(report) = config.section("report")? {
            report.only(&["file"])?;
            defaults.report = report.text("file")?.map(PathBuf::from);
        }
        if let Some(retry) = config.section("retry")? {
            retry.only(&["machine_count", "base_delay_ms"])?;
            defaults.machine_retries = retry
                .integer("machine_count")?
                .map(|number| {
                    u32::try_from(number)
                        .map_err(|_| retry.invalid("machine_count", "use a whole number from zero"))
                })
                .transpose()?;
            defaults.retry_delay_ms = retry
                .integer("base_delay_ms")?
                .map(|number| {
                    u64::try_from(number)
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| retry.invalid("base_delay_ms", "use a number above zero"))
                })
                .transpose()?;
        }
        Ok(defaults)
    }

    /// Applies executor defaults to runs and checks; graph commands keep the simulator.
    /// Explicit executor and report options override configuration defaults.
    pub fn apply(self, command: &str, arguments: &mut Arguments) -> Result<(), Failure> {
        if matches!(
            command,
            "run" | "graph" | "check-order" | "repeat" | "replay"
        ) {
            arguments.machine_retries = arguments.machine_retries.or(self.machine_retries);
            arguments.retry_delay_ms = arguments.retry_delay_ms.or(self.retry_delay_ms);
            arguments.task_seconds = arguments.task_seconds.or(self.task_seconds);
        }
        if matches!(command, "run" | "check" | "check-machine")
            && arguments.executor.is_none()
            && arguments.preset.is_none()
        {
            arguments.executor = self.executor.or_else(|| Some("local".into()));
            arguments.executor_default = true;
        } else if matches!(command, "graph" | "check-order" | "inputs" | "outputs")
            && arguments.executor.is_none()
            && arguments.preset.is_none()
        {
            arguments.executor = Some("simulator".into());
        }
        if command == "run" && arguments.report.is_none() {
            arguments.report = self.report;
        }
        if let Some(folder) = self.cache_folder {
            crate::use_cache_folder(folder);
        }
        storage::use_proxy(self.proxy.as_deref())
            .map_err(|error| Failure::setup(format!("uwe: {error}")))
    }
}

impl Setup {
    /// Reads the config files.
    pub fn load(arguments: &Arguments) -> Result<Setup, Failure> {
        let failed = |error: ConfigError| Failure::setup(format!("uwe: {error}"));
        let config = ConfigFile::load(arguments.config_file.as_deref()).map_err(failed)?;
        let config = config.with_secrets(arguments.secrets.clone());
        config.root().only(SECTIONS).map_err(failed)?;
        let registry = executor::credentials::Registry::from_config(&config).map_err(failed)?;
        let storage = Storage::from_config(&config).map_err(failed)?;
        let executors = Executors::read(&config).map_err(failed)?;
        let telemetry = Telemetry::read(&config).map_err(failed)?;
        let network = network(&config).map_err(failed)?;
        let conda = crate::conda::Config::read(&config).map_err(failed)?;
        let cwl_registry = cwl_registry(&config).map_err(failed)?;
        let mut secrets = std::collections::BTreeMap::new();
        if let Some(section) = config.section("secrets").map_err(failed)? {
            for name in section.keys() {
                if let Some(secret) = section.secret(name).map_err(failed)? {
                    if secret.variable().is_none() {
                        return Err(failed(section.invalid(
                            name,
                            "secrets require an environment variable reference",
                        )));
                    }
                    secrets.insert(name.to_owned(), secret);
                }
            }
        }
        Ok(Setup {
            storage,
            registry,
            executors,
            telemetry,
            network,
            conda,
            secrets,
            cwl_registry,
            ..Setup::default()
        })
    }

    /// The container engine as `UWE_ENGINE` names it, else as `[executor.local] engine`
    /// does; empty for Docker, else Podman.
    pub fn engine(&self) -> String {
        (std::env::var("UWE_ENGINE")
            .ok()
            .filter(|name| !name.is_empty()))
        .or_else(|| self.executors.engine.clone())
        .unwrap_or_default()
    }

    /// Names the task engine for Nextflow metadata and config scope selection.
    /// Uses the runtime of the cluster nodes or this machine's Docker/Podman engine.
    pub fn engine_kind(&self, arguments: &Arguments) -> String {
        let nodes = |config: &crate::grid::Config| config.container.clone();
        let runtime = match arguments.executor.as_deref() {
            Some("htcondor") => nodes(&self.executors.htcondor),
            Some("pbs") => nodes(&self.executors.pbs),
            _ => Some(self.slurm_container(arguments).to_owned()),
        };
        let runtime = runtime.unwrap_or_else(|| "apptainer".to_owned());
        engine_kind(arguments.executor.as_deref(), &runtime, &self.engine())
    }

    /// The container runtime of SLURM jobs: `-slurm-container`, else `container` of
    /// `[executor.slurm]`, else `apptainer`.
    pub fn slurm_container<'a>(&'a self, arguments: &'a Arguments) -> &'a str {
        (arguments.slurm_container.as_deref())
            .or(self.executors.slurm_container.as_deref())
            .unwrap_or("apptainer")
    }

    /// Where the cache store lives: `-store`, else `location` of `[store]`, else `store` in
    /// the cache directory.
    pub fn store_location(&self, arguments: &Arguments) -> String {
        (arguments.store.clone())
            .or_else(|| self.storage.store_location().map(str::to_owned))
            .unwrap_or_else(|| {
                let folder = crate::cache_directory().join("store");
                folder.to_string_lossy().into_owned()
            })
    }

    /// The cache store: a folder, or a place of a storage provider that can write there. With
    /// `local` of `[store]`, a folder of this machine stands in front of a store elsewhere:
    /// reads look there first, and new results go to both.
    pub fn store(&self, arguments: &Arguments) -> Result<Store, Failure> {
        let location = self.store_location(arguments);
        let failed = |message: String| Failure::setup(format!("uwe: {message}"));
        let place = (self.storage.store(&location)).map_err(|error| failed(error.to_string()))?;
        let store = match place {
            StoreLocation::Folder(folder) => Store::local(&folder),
            StoreLocation::Objects(operator) => Store::remote(operator, &location),
        };
        let mut store = store
            .map_err(|error| failed(format!("cannot open the store at {location}: {error}")))?;
        if store.local_path("").is_none() {
            store = store.with_staging(crate::cache_directory().join("imports"));
        }
        let Some(local) = self.storage.local_store() else {
            return Ok(store);
        };
        let opened = Store::local(std::path::Path::new(local))
            .and_then(|front| Store::layered(front, store));
        opened.map_err(|error| {
            failed(format!(
                "cannot open the folder {local} in front of the store: {error}; name another with \
                 `local` in [store]"
            ))
        })
    }
}

/// The kind of the engine that runs the tasks of `executor`: the runtime of the cluster nodes
/// `slurm`, or the engine of this machine, `local`.
fn engine_kind(executor: Option<&str>, slurm: &str, local: &str) -> String {
    match executor {
        Some("slurm" | "htcondor" | "pbs") => slurm.to_owned(),
        _ if local.contains("podman") => "podman".to_owned(),
        _ => "docker".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use config_file::ConfigFile;
    use slurm_executor::PullResources;

    use super::Executors;
    use task_runner::NetworkRule;

    #[test]
    fn loads_object_staging() {
        let settings = read("[executor.tes]\nserver = 'https://tes.example.org'\nstaging = 'object-store'\nstaging_root = 's3://bucket/tes'\nidempotency_tag = 'example.org/key'\nuser = { environment = 'TES_USER' }\npassword = { environment = 'TES_PASSWORD' }\nmax_inputs = 200\n").unwrap();
        assert_eq!(
            settings.tes.staging_root.as_deref(),
            Some("s3://bucket/tes")
        );
        assert_eq!(
            settings.tes.idempotency_tag.as_deref(),
            Some("example.org/key")
        );
        assert_eq!(settings.tes.max_inputs, 200);
        assert_eq!(
            settings.tes.user.as_ref().unwrap().variable(),
            Some("TES_USER")
        );
        assert_eq!(
            settings.tes.password.as_ref().unwrap().variable(),
            Some("TES_PASSWORD")
        );
        assert!(read("[executor.tes]\nstaging = 'object-store'\n").is_err());
    }

    #[test]
    fn retry_options_override() {
        let config = ConfigFile::parse(
            "[retry]\nmachine_count = 4\nbase_delay_ms = 250\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let mut arguments = crate::arguments::Arguments {
            machine_retries: Some(0),
            ..Default::default()
        };
        super::Defaults::read(&config)
            .unwrap()
            .apply("run", &mut arguments)
            .unwrap();
        assert_eq!(arguments.machine_retries, Some(0));
        assert_eq!(arguments.retry_delay_ms, Some(250));
        for setting in ["machine_count = -1", "base_delay_ms = 0"] {
            let config =
                ConfigFile::parse(&format!("[retry]\n{setting}\n"), Path::new("config.toml"))
                    .unwrap();
            assert!(super::Defaults::read(&config).is_err());
        }
    }

    #[test]
    fn exposes_task_engine() {
        let kind = super::engine_kind;
        assert_eq!(kind(Some("slurm"), "apptainer", "docker"), "apptainer");
        assert_eq!(kind(Some("slurm"), "singularity", ""), "singularity");
        assert_eq!(kind(Some("local"), "apptainer", ""), "docker");
        assert_eq!(
            kind(None, "apptainer", "/run/user/1/podman/podman.sock"),
            "podman"
        );
    }

    fn read(text: &str) -> Result<Executors, String> {
        let config =
            ConfigFile::parse(text, Path::new("config.toml")).map_err(|e| e.to_string())?;
        Executors::read(&config).map_err(|error| error.to_string())
    }

    #[test]
    fn loads_executor_settings() {
        let text = "[executor.local]\nengine = \"podman\"\n\n[executor.tes]\nserver = \
                    \"https://tes.example.org\"\ntoken = \"t\"\n\n[executor.slurm]\ncontainer = \
                    \"podman\"\noptions = [\"--account=lab\", \"--qos=low\"]\n\n\
                    [executor.kubernetes]\nkubeconfig = \"/k/kind.yaml\"\nnamespace = \"x\"\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.engine.as_deref(), Some("podman"));
        assert_eq!(
            settings.tes_server.as_deref(),
            Some("https://tes.example.org")
        );
        let token = settings
            .tes_token
            .expect("a token")
            .reveal()
            .expect("its text");
        assert_eq!(token, "t");
        assert_eq!(settings.slurm_container.as_deref(), Some("podman"));
        let options = settings.slurm_options.expect("options");
        assert_eq!(options, ["--account=lab", "--qos=low"]);
        assert_eq!(settings.slurm_pull, PullResources::default());
        assert_eq!(
            settings.kubeconfig.as_deref(),
            Some(Path::new("/k/kind.yaml"))
        );
        assert_eq!(settings.kubernetes_namespace.as_deref(), Some("x"));
        assert!(settings.tes_sites.is_empty());
        assert!(read("").expect("no settings").engine.is_none());
    }

    #[test]
    fn loads_remote_staging() {
        let text = "[executor.slurm]\nstaging = 'object-store'\nstaging_root = 's3://b/s'\n\
                    helper_url = 'https://example.org/uwe'\nhelper_sha256 = '";
        let settings = read(&format!("{text}{}'\n", "AB".repeat(32))).expect("settings");
        let helper = settings.slurm_helper.expect("a helper");
        assert!(
            matches!(helper, slurm_executor::Helper::Download { sha256, .. }
            if sha256 == "ab".repeat(32))
        );
        assert!(read(&format!("{text}ab'\n")).is_err(), "a short digest");
        let text = format!(
            "{text}{}'\nstaging_access = 'credentials'\n",
            "ab".repeat(32)
        );
        assert!(read(&text).unwrap_err().contains("staging_secret_file"));
        let text = format!("{text}staging_secret_file = '/home/me/staging.toml'\n");
        let file = read(&text).expect("settings").slurm_secret_file;
        assert_eq!(file.as_deref(), Some(Path::new("/home/me/staging.toml")));
        assert!(read(&format!("{text}helper_file = '/a'\n")).is_err());
        // Without a helper setting the jobs get the running binary, or the file of helper_file.
        let text = "[executor.slurm]\nstaging = 'object-store'\nstaging_root = 's3://b'\n";
        let settings = read(text).expect("settings");
        assert!(settings.slurm_helper.is_none());
        let current = std::env::current_exe().expect("this program");
        assert_eq!(settings.slurm_helper_file, Some(current));
        let file = read(&format!("{text}helper_file = '/opt/uwe-arm64'\n"));
        let file = file.expect("settings").slurm_helper_file;
        assert_eq!(file.as_deref(), Some(Path::new("/opt/uwe-arm64")));
        assert!(
            read("[executor.slurm]\n")
                .expect("settings")
                .slurm_helper_file
                .is_none()
        );
        let text = "[executor.kubernetes]\nstaging = 'object-store'\nstaging_root = 's3://b'\n\
                    helper_image = 'helper:1'\nhelper_mount = 'init-container'\n";
        let helper = read(text)
            .expect("settings")
            .kubernetes_helper
            .expect("a helper");
        assert_eq!(helper.image, "helper:1");
        assert!(!helper.image_volume);
        let text = format!("{text}staging_access = 'credentials'\n");
        assert!(read(&text).unwrap_err().contains("staging_secret"));
        let text = format!("{text}staging_secret = 'uwe-staging'\n");
        let helper = read(&text).expect("settings").kubernetes_helper;
        assert_eq!(
            helper.expect("a helper").secret.as_deref(),
            Some("uwe-staging")
        );
        assert!(read(&format!("{text}helper_file = '/a'\n")).is_err());
        assert!(read(&format!("{text}helper_download_image = 'b'\n")).is_err());
        // Without helper_image the pods download the running binary with Alpine.
        let text = "[executor.kubernetes]\nstaging = 'object-store'\nstaging_root = 's3://b'\n";
        let settings = read(text).expect("settings");
        let helper = settings.kubernetes_helper.expect("a helper");
        assert_eq!(helper.image, kubernetes_executor::DOWNLOAD_IMAGE);
        let current = std::env::current_exe().expect("this program");
        assert_eq!(settings.kubernetes_helper_file, Some(current));
        let text = format!(
            "{text}helper_file = '/opt/uwe-arm64'\nhelper_download_image = 'mirror/busybox'\n"
        );
        let settings = read(&text).expect("settings");
        assert_eq!(
            settings.kubernetes_helper.expect("a helper").image,
            "mirror/busybox"
        );
        let file = settings.kubernetes_helper_file;
        assert_eq!(file.as_deref(), Some(Path::new("/opt/uwe-arm64")));
        assert!(read(&format!("{text}helper_mount = 'init-container'\n")).is_err());
        let text = "[executor.kubernetes.shared_volume]\nnfs_server = 'nfs.example.org'\n\
                    nfs_path = '/export'\npath = '/mnt/uwe'\n";
        let volumes = read(text).expect("settings").kubernetes_volumes;
        let kubernetes_executor::Volumes::Under { root, source } = volumes else {
            panic!("one volume");
        };
        assert_eq!(root, Path::new("/mnt/uwe"));
        assert_eq!(source["nfs"]["server"], "nfs.example.org");
        let claim = "[executor.kubernetes.shared_volume]\nclaim = 'cache'\nnfs_path = '/x'\n\
                     path = '/mnt'\n";
        assert!(read(claim).is_err(), "a claim and an export");
    }

    #[test]
    fn loads_slurm_resources() {
        let text = "[executor.slurm.pull]\ncpus = 8\nminutes = 120\n";
        let pull = read(text).expect("settings").slurm_pull;
        let expected = PullResources {
            cpus: 8,
            time_seconds: 7200,
            ..PullResources::default()
        };
        assert_eq!(pull, expected);
        let error = read("[executor.slurm.pull]\nmemory_mb = 0\n").expect_err("an error");
        assert!(error.contains("use a number above 0"), "{error}");
        let error = read("[executor.slurm.pull]\nmemory = 8\n").expect_err("an error");
        assert!(error.contains("memory"), "{error}");
        let text = "[executor.kubernetes]\nunschedulable_minutes = 90\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.kubernetes_unschedulable, Some(90));
        assert!(read("[executor.kubernetes]\nunschedulable_minutes = 0\n").is_err());
        let settings =
            read("[executor.kubernetes]\nstart_grace_seconds = 300\n").expect("settings");
        assert_eq!(settings.kubernetes_start_grace, Some(300));
        assert!(read("[executor.kubernetes]\nstart_grace_seconds = 5\n").is_err());
        assert!(read("[executor.kubernetes]\nstart_grace_seconds = 3601\n").is_err());
        let settings = read("[executor.slurm]\nquery_seconds = 120\n").expect("settings");
        assert_eq!(settings.slurm_query, Some(120));
        assert!(read("[executor.slurm]\nquery_seconds = 0\n").is_err());
        let text = "[executor.kubernetes]\ncontext = 'lab'\nservice_account = 'runner'\n\
                    pod = [{ nodeSelector = 'disktype=ssd' }]\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.kubernetes_context.as_deref(), Some("lab"));
        assert_eq!(
            settings.kubernetes_pod.service_account.as_deref(),
            Some("runner")
        );
        let text = "[executor.kubernetes.cws]\naddress = 'http://cws.lab:8080/'\n\
                    strategy = 'rank_max-fair'\n";
        let cws = read(text)
            .expect("settings")
            .kubernetes_cws
            .expect("a scheduler");
        assert_eq!(cws.address, "http://cws.lab:8080");
        assert_eq!(cws.strategy, "rank_max-fair");
        assert_eq!(cws.scheduler_name, "workflow-scheduler");
        assert!(read("[executor.kubernetes.cws]\nstrategy = 'fifo'\n").is_err());
        let error = read("[executor.kubernetes]\npod = [{ privileged = true }]\n");
        assert!(
            error
                .expect_err("refused")
                .contains("never starts privileged")
        );
        let settings = read("[executor.slurm]\nmemory_per_cpu_gb = 4.5\n").expect("settings");
        assert_eq!(settings.slurm_cpu_memory, Some(4.5));
        assert!(read("[executor.slurm]\nmemory_per_cpu_gb = 0\n").is_err());
    }

    #[test]
    fn loads_network_policy() {
        let parse = |text: &str| -> Result<NetworkRule, String> {
            let config = ConfigFile::parse(text, Path::new("config.toml"))
                .map_err(|error| error.to_string())?;
            super::network(&config).map_err(|error| error.to_string())
        };
        let rule = parse("[network]\ndefault = \"none\"\nallowed_steps = [\"DOWNLOAD_*\"]\n")
            .expect("a rule");
        assert!(rule.none_by_default);
        assert_eq!(rule.allowed_steps, ["DOWNLOAD_*"]);
        assert_eq!(parse("").expect("no rule"), NetworkRule::default());
        let error = parse("[network]\ndefault = \"off\"\n").expect_err("an error");
        assert!(error.contains(":2:") && error.contains("off"), "{error}");
    }

    #[test]
    fn loads_cwl_registry() {
        let parse = |text: &str| -> Result<Option<String>, String> {
            let config = ConfigFile::parse(text, Path::new("config.toml"))
                .map_err(|error| error.to_string())?;
            super::cwl_registry(&config).map_err(|error| error.to_string())
        };
        let registry = parse("[cwl]\nregistry = \"registry.example.org/team/cwl\"\n");
        assert_eq!(registry, Ok(Some("registry.example.org/team/cwl".into())));
        assert_eq!(parse(""), Ok(None));
        let error = parse("[cwl]\nregistry = \"registry.example.org/cwl:1\"\n").expect_err("a tag");
        assert!(error.contains("registry"), "{error}");
    }

    #[test]
    fn loads_tes_sites() {
        let text = "[executor.tes]\nserver = \"https://tes.example.org\"\n\n\
                    [executor.tes.sites.hamburg]\nserver = \"https://tes.hh.example.org\"\n\
                    near = [\"s3://hh/\", \"/mnt/hh/\"]\n\n\
                    [executor.tes.sites.heidelberg]\nserver = \"https://tes.hd.example.org\"\n";
        let sites = read(text).expect("settings").tes_sites;
        let names: Vec<&str> = sites.iter().map(|site| site.name.as_str()).collect();
        assert_eq!(names, ["hamburg", "heidelberg"]);
        assert_eq!(sites[0].server, "https://tes.hh.example.org");
        assert_eq!(sites[0].near, ["s3://hh/", "/mnt/hh/"]);
        assert!(sites[1].near.is_empty() && sites[1].token.is_none());
        let error = read("[executor.tes.sites.x]\nnear = [\"/a/\"]\n").expect_err("an error");
        assert!(error.contains("server"), "{error}");
    }

    fn defaults(text: &str) -> Result<super::Defaults, String> {
        let config =
            ConfigFile::parse(text, Path::new("config.toml")).map_err(|e| e.to_string())?;
        super::Defaults::read(&config).map_err(|error| error.to_string())
    }

    #[test]
    fn loads_command_defaults() {
        let text = "[executor]\ndefault = \"slurm\"\n\n[cache]\nfolder = \"/vol/cache\"\n\n\
                    [network]\nproxy = \"http://proxy.example.org:3128\"\n\n\
                    [report]\nfile = \"report.html\"\n";
        let found = defaults(text).expect("defaults");
        assert_eq!(found.executor.as_deref(), Some("slurm"));
        assert_eq!(found.cache_folder.as_deref(), Some(Path::new("/vol/cache")));
        assert_eq!(
            found.proxy.as_deref(),
            Some("http://proxy.example.org:3128")
        );
        assert_eq!(found.report.as_deref(), Some(Path::new("report.html")));
        assert_eq!(defaults("").expect("none"), super::Defaults::default());
        let error = defaults("[report]\nfolder = \"r\"\n").expect_err("an error");
        assert!(error.contains("folder"), "{error}");
        let error = defaults("[executor]\ndefault = \"cloud\"\n").expect_err("an error");
        assert!(error.contains(":2:") && error.contains("cloud"), "{error}");
        let error = defaults("[cache]\nfolder = \"cache\"\n").expect_err("an error");
        assert!(error.contains("absolute"), "{error}");
        let error = defaults("[cache]\nplace = \"/c\"\n").expect_err("an error");
        assert!(error.contains("place"), "{error}");
        // `default` is no unknown executor for the settings of the executors.
        assert!(read("[executor]\ndefault = \"slurm\"\n").is_ok());
        let timed = "[executor]\ntask_minutes = 90\n";
        assert_eq!(defaults(timed).expect("a time").task_seconds, Some(5400));
        assert!(read(timed).is_ok());
        let error = defaults("[executor]\ntask_minutes = 0\n").expect_err("an error");
        assert!(error.contains("task_minutes"), "{error}");
    }

    #[test]
    fn executor_option_overrides() {
        let slurm = || super::Defaults {
            executor: Some("slurm".into()),
            ..super::Defaults::default()
        };
        for command in ["run", "check"] {
            let mut arguments = crate::arguments::parse(&["main.nf".into()]).expect("arguments");
            slurm().apply(command, &mut arguments).unwrap();
            assert_eq!(arguments.executor.as_deref(), Some("slurm"));
        }
        let words = ["main.nf".into(), "-executor".into(), "local".into()];
        let mut arguments = crate::arguments::parse(&words).expect("arguments");
        slurm().apply("run", &mut arguments).unwrap();
        assert_eq!(arguments.executor.as_deref(), Some("local"));
        let mut arguments = crate::arguments::parse(&["main.nf".into()]).expect("arguments");
        slurm().apply("graph", &mut arguments).unwrap();
        assert_eq!(arguments.executor.as_deref(), Some("simulator"));
        for command in ["run", "check", "check-machine"] {
            let mut arguments = crate::arguments::Arguments::default();
            super::Defaults::default()
                .apply(command, &mut arguments)
                .unwrap();
            assert_eq!(arguments.executor.as_deref(), Some("local"));
        }
        for command in ["graph", "check-order", "inputs", "outputs"] {
            let mut arguments = crate::arguments::Arguments::default();
            slurm().apply(command, &mut arguments).unwrap();
            assert_eq!(arguments.executor.as_deref(), Some("simulator"));
        }
    }

    #[test]
    fn rejects_unknown_settings() {
        let error = read("[executor.tes]\nsrever = \"x\"\n").expect_err("an error");
        assert!(error.contains("srever"), "{error}");
        let error = read("[executor.cloud]\nx = 1\n").expect_err("an error");
        assert!(error.contains("cloud"), "{error}");
    }
}
