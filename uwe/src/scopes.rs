//! Fills executor settings that the config file of uwe leaves unset from the scopes of a
//! Nextflow config: `nomad`, `k8s`, `slurm_no_memory`, `ospool`, `tes` and `tencentcloud`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config_file::{Origin, Secret};
use nextflow_config::Config;
use nextflow_evaluator::value::Value;
use serde_json::Value as Json;
use storage::settings::{Kind, ProviderSettings, S3};

use crate::nextflow::text;

/// Reads the values of a Nextflow config by their path.
struct Scope<'a>(&'a Config);

impl Scope<'_> {
    fn get(&self, path: &[&str]) -> Option<Value> {
        (self.0.get(path)).filter(|value| !matches!(value, Value::Null))
    }

    fn text(&self, path: &[&str]) -> Option<String> {
        self.get(path).as_ref().and_then(text)
    }

    fn flag(&self, path: &[&str]) -> Option<bool> {
        self.text(path).map(|text| text == "true")
    }

    fn number(&self, path: &[&str]) -> Option<u64> {
        self.text(path).and_then(|text| text.trim().parse().ok())
    }

    fn texts(&self, path: &[&str]) -> Vec<String> {
        match self.get(path) {
            Some(Value::List(list)) => list.borrow().items.iter().filter_map(text).collect(),
            Some(value) => text(&value)
                .map(|text| text.split(',').map(|part| part.trim().to_owned()).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// A duration, from Nextflow's duration values or text like `30 sec`.
    fn duration(&self, path: &[&str]) -> Option<Duration> {
        let millis = match self.get(path)? {
            Value::Duration(millis) => millis,
            value => nextflow_evaluator::units::parse_duration(&text(&value)?).ok()?,
        };
        u64::try_from(millis).ok().map(Duration::from_millis)
    }

    /// The value as JSON; `None` without one, `Some(Null)` for a closure, which has no JSON.
    fn json(&self, path: &[&str]) -> Option<Json> {
        self.get(path)
            .map(|value| nextflow_runtime::inputs::to_json(&value))
    }
}

/// Fills the Nomad settings from the `nomad` scope of nf-nomad, and nf-nomad-s5cmd's staging
/// from `nomad.s5cmd`; gives the S3 provider of the staging bucket that `nomad.s5cmd.s3` names.
/// Privileged containers are refused.
pub(crate) fn nomad(
    config: &Config,
    nomad: &mut crate::nomad::Config,
) -> Result<Option<ProviderSettings>, String> {
    use nomad_executor::{Cleanup, CpuMode, Hidden, PRIVILEGED};
    let scope = Scope(config);
    let jobs = |key: &'static str| ["nomad", "jobs", key];
    let client = |key: &'static str| ["nomad", "client", key];
    if scope.flag(&jobs("privileged")) == Some(true) {
        return Err(format!("nomad.jobs.privileged: {PRIVILEGED}"));
    }
    let settings = &mut nomad.settings;
    let fill = |slot: &mut Option<String>, path: &[&str]| {
        if slot.is_none() {
            *slot = scope.text(path);
        }
    };
    fill(&mut settings.address, &client("address"));
    fill(&mut settings.namespace, &jobs("namespace"));
    fill(&mut settings.region, &jobs("region"));
    fill(&mut settings.node_pool, &jobs("nodePool"));
    fill(&mut settings.network_mode, &jobs("networkMode"));
    if settings.token.is_none() {
        settings.token = scope.text(&client("token")).map(Hidden::Text);
    }
    if settings.datacenters.is_empty() {
        settings.datacenters = scope.texts(&jobs("datacenters"));
    }
    let wrong = |path: &[&str], message: String| format!("{}: {message}", path.join("."));
    if let Some(driver) = scope
        .text(&jobs("driver"))
        .filter(|_| settings.driver == "docker")
    {
        let path = jobs("driver");
        settings.driver =
            nomad_executor::driver_name(&driver).map_err(|text| wrong(&path, text))?;
    }
    if let Some(mode) = scope
        .text(&jobs("cpuMode"))
        .filter(|_| settings.cpu_mode == CpuMode::Cores)
    {
        let path = jobs("cpuMode");
        settings.cpu_mode = nomad_executor::cpu_mode(&mode).map_err(|text| wrong(&path, text))?;
    }
    if let Some(device) = scope.text(&jobs("acceleratorDeviceName")) {
        settings.device = device;
    }
    if let Some(Json::Object(meta)) = scope.json(&jobs("meta")) {
        for (key, value) in meta {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            settings.meta.entry(key).or_insert(value);
        }
    }
    let rules = [
        (
            "constraints",
            nomad_executor::constraint as nomad_executor::Rule,
        ),
        ("affinities", nomad_executor::affinity),
        ("spreads", nomad_executor::spread),
    ];
    for (key, read) in rules {
        let slot = match key {
            "constraints" => &mut settings.constraints,
            "affinities" => &mut settings.affinities,
            _ => &mut settings.spreads,
        };
        match scope.json(&jobs(key)) {
            _ if !slot.is_empty() => {}
            None => {}
            Some(Json::Null) => tracing::warn!(
                "uwe ignores nomad.jobs.{key} given as a closure; give them as a list of maps \
                 with attribute, operator and value, or in [executor.nomad]"
            ),
            Some(list) => {
                let path = jobs(key);
                *slot =
                    nomad_executor::rules(Some(list), read).map_err(|text| wrong(&path, text))?;
            }
        }
    }
    if settings.volumes.is_empty()
        && let Some(volumes) = scope.json(&jobs("volumes"))
    {
        let path = jobs("volumes");
        settings.volumes =
            nomad_executor::volume_list(&volumes).map_err(|text| wrong(&path, text))?;
    }
    if let Some(count) = scope.number(&jobs("restartAttempts")) {
        settings.restart_attempts = u32::try_from(count).unwrap_or(u32::MAX);
    }
    if let Some(count) = scope.number(&jobs("rescheduleAttempts")) {
        settings.reschedule_attempts = u32::try_from(count).unwrap_or(u32::MAX);
    }
    if let Some(cleanup) = scope.text(&jobs("cleanup")) {
        let path = jobs("cleanup");
        settings.cleanup =
            nomad_executor::cleanup_mode(&cleanup).map_err(|text| wrong(&path, text))?;
    } else if let Some(delete) = scope.flag(&jobs("deleteOnCompletion")) {
        settings.cleanup = if delete {
            Cleanup::Always
        } else {
            Cleanup::Never
        };
    }
    settings.fail_unplaced |= scope.flag(&jobs("failOnPlacementFailure")) == Some(true);
    if let Some(timeout) = scope.duration(&jobs("placementFailureTimeout")) {
        settings.placement = timeout.max(Duration::from_secs(1));
    }
    if let Some(poll) = scope
        .duration(&client("pollInterval"))
        .filter(|poll| poll.as_secs() >= 1)
    {
        settings.poll = poll;
    }
    for (key, why) in [
        ("secrets", "it gives tasks the secrets of the run itself"),
        ("dockerVolume", "it mounts the volumes of `volumes`"),
    ] {
        if scope.get(&jobs(key)).is_some() {
            tracing::warn!("uwe ignores nomad.jobs.{key}: {why}");
        }
    }
    if scope.get(&["nomad", "debug"]).is_some() {
        tracing::warn!("uwe ignores nomad.debug: it keeps its own records of each attempt");
    }
    s5cmd(&scope, nomad)
}

/// The staging root and S3 provider of nf-nomad-s5cmd's distributed work dir, which uwe's
/// object-store staging and helper replace; the `s5cmd` program is not needed.
fn s5cmd(
    scope: &Scope<'_>,
    nomad: &mut crate::nomad::Config,
) -> Result<Option<ProviderSettings>, String> {
    let key = |path: &[&'static str]| {
        let mut full = vec!["nomad", "s5cmd"];
        full.extend_from_slice(path);
        full
    };
    if scope.flag(&key(&["enabled"])) != Some(true) {
        return Ok(None);
    }
    tracing::warn!(
        "uwe stages the files of Nomad jobs with its own helper through the staging root, not \
         with s5cmd: it uses nomad.s5cmd.workDir and nomad.s5cmd.s3, and ignores the other \
         nomad.s5cmd settings and the s5cmd host volume"
    );
    let bucket = scope.text(&key(&["workDir", "bucket"]));
    let distributed = scope.flag(&key(&["workDir", "enabled"])) == Some(true);
    let Some(bucket) = bucket.filter(|_| distributed) else {
        // File staging mode: uwe's providers read `s3://` inputs themselves.
        return Ok(None);
    };
    let bucket = bucket
        .trim_start_matches("s3://")
        .trim_end_matches('/')
        .to_owned();
    let prefix = scope.text(&key(&["workDir", "prefix"])).unwrap_or_default();
    let prefix = prefix.trim_matches('/');
    if nomad.staging.root.is_none() {
        nomad.staging.root = Some(match prefix.is_empty() {
            true => format!("s3://{bucket}"),
            false => format!("s3://{bucket}/{prefix}"),
        });
        nomad.staging.mode = crate::staging::Mode::ObjectStore;
        if nomad.settings.helper.is_none() && nomad.helper_file.is_none() {
            nomad.helper_file = std::env::current_exe().ok();
        }
    }
    let s3 = |name: &'static str| scope.text(&key(&["s3", name]));
    let secret = |name: &'static str| s3(name).map(|value| Secret::written(value, origin()));
    let settings = S3 {
        endpoint: s3("endpoint"),
        region: s3("region").or_else(|| Some("us-east-1".into())),
        path_style: scope.flag(&key(&["s3", "usePathStyle"])),
        access_key_id: secret("accessKeyId"),
        secret_access_key: secret("secretAccessKey"),
        ..S3::default()
    };
    Ok(Some(ProviderSettings {
        name: "nomad.s5cmd".into(),
        prefixes: vec![format!("s3://{bucket}/")],
        kind: Kind::S3(settings),
        origin: origin(),
    }))
}

/// Fills the Kubernetes settings from Nextflow's `k8s` scope: namespace, context, service
/// account, pod options and a shared claim. Privileged pods are refused.
pub(crate) fn kubernetes(
    config: &Config,
    executors: &mut crate::setup::Executors,
) -> Result<(), String> {
    let scope = Scope(config);
    let key = |name: &'static str| ["k8s", name];
    if executors.kubernetes_namespace.is_none() {
        executors.kubernetes_namespace = scope.text(&key("namespace"));
    }
    if executors.kubernetes_context.is_none() {
        executors.kubernetes_context = scope.text(&key("context"));
    }
    let pod = &mut executors.kubernetes_pod;
    if pod.service_account.is_none() {
        pod.service_account = scope.text(&key("serviceAccount"));
    }
    let mut options = Vec::new();
    if let Some(list) = scope.json(&key("pod")) {
        options.push(list);
    }
    if let Some(policy) = scope.text(&key("imagePullPolicy")) {
        options.push(serde_json::json!({"imagePullPolicy": policy}));
    }
    if let Some(user) = scope.number(&key("runAsUser")) {
        options.push(serde_json::json!({"runAsUser": user}));
    }
    if let Some(context @ Json::Object(_)) = scope.json(&key("securityContext")) {
        options.push(serde_json::json!({"securityContext": context}));
    }
    for list in options {
        pod.add(&list)
            .map_err(|message| format!("k8s: {message}"))?;
    }
    let shared = executors.kubernetes_staging.mode == crate::staging::Mode::SharedPath;
    let host = matches!(
        executors.kubernetes_volumes,
        kubernetes_executor::Volumes::Host
    );
    if let Some(claim) = scope
        .text(&key("storageClaimName"))
        .filter(|_| shared && host)
    {
        let path = scope
            .text(&key("storageMountPath"))
            .unwrap_or_else(|| "/workspace".into());
        executors.kubernetes_volumes = kubernetes_executor::Volumes::Under {
            root: PathBuf::from(path),
            source: serde_json::json!({"persistentVolumeClaim": {"claimName": claim}}),
        };
    }
    for (name, why) in [
        ("client", "it reads the cluster from the kubeconfig"),
        (
            "storageSubPath",
            "it mounts the claim of storageClaimName whole",
        ),
    ] {
        if scope.get(&key(name)).is_some() {
            tracing::warn!("uwe ignores k8s.{name}: {why}");
        }
    }
    if scope.flag(&key("autoMountHostPaths")) == Some(true) {
        tracing::warn!(
            "uwe ignores k8s.autoMountHostPaths: it mounts the files of each task itself"
        );
    }
    if scope.text(&key("computeResourceType")).as_deref() == Some("Pod") {
        tracing::warn!(
            "uwe runs each task as a Kubernetes Job, also with k8s.computeResourceType = 'Pod'"
        );
    }
    Ok(())
}

/// Where the values of a Nextflow config come from, for errors of secrets.
fn origin() -> Origin {
    Origin {
        file: Arc::from(Path::new("nextflow.config")),
        line: 0,
    }
}

/// Fills the TES settings from nf-ga4gh's `tes` scope, and for nf-tencentcloud's executor
/// `tencentcloud-omics` from `tencentcloud.omics`. A Nextflow endpoint gets `/v1`, as there.
pub(crate) fn tes(config: &Config, executors: &mut crate::setup::Executors) -> Result<(), String> {
    let scope = Scope(config);
    let secret = |path: &[&str]| {
        scope
            .text(path)
            .map(|value| Secret::written(value, origin()))
    };
    let key = |name: &'static str| ["tes", name];
    if executors.tes_server.is_none() {
        let endpoint = scope.text(&key("endpoint"));
        executors.tes_server =
            endpoint.map(|endpoint| format!("{}/v1", endpoint.trim_end_matches('/')));
    }
    if executors.tes_token.is_none() {
        executors.tes_token = secret(&key("oauthToken"));
    }
    let settings = &mut executors.tes;
    let (user, password) = (secret(&key("basicUsername")), secret(&key("basicPassword")));
    if settings.user.is_none() && user.is_some() && password.is_some() {
        (settings.user, settings.password) = (user, password);
    }
    let (api_key, name) = (secret(&key("apiKey")), scope.text(&key("apiKeyParamName")));
    if settings.api_key.is_none() && api_key.is_some() && name.is_some() {
        (settings.api_key, settings.api_key_name) = (api_key, name);
        settings.api_key_header = scope.text(&key("apiKeyParamMode")).as_deref() == Some("header");
    }
    if let Some(seconds) = scope.number(&key("timeout")).filter(|seconds| *seconds > 0) {
        settings.timeout.get_or_insert(Duration::from_secs(seconds));
    }
    if let Some(Json::Object(tags)) = scope.json(&key("tags")) {
        for (name, value) in tags {
            if name.starts_with("uwe-") {
                return Err(format!("tes.tags.{name} is a tag that uwe sets itself"));
            }
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            settings.tags.entry(name).or_insert(value);
        }
    }
    if !names_executor(config, "tencentcloud-omics") {
        return Ok(());
    }
    let omics = |name: &'static str| ["tencentcloud", "omics", name];
    let endpoint = scope.text(&omics("endpoint"));
    let endpoint = endpoint.ok_or("tencentcloud.omics.endpoint must be specified!")?;
    let endpoint = endpoint.trim_end_matches('/');
    executors.tes_server = Some(match endpoint.ends_with("/v1") {
        true => endpoint.to_owned(),
        false => format!("{endpoint}/v1"),
    });
    let token = secret(&omics("accessToken"));
    let token = token.ok_or("tencentcloud.omics.accessToken must be specified!")?;
    executors.tes_token.get_or_insert(token);
    executors
        .tes
        .platform
        .get_or_insert(tes_executor::Platform::TencentOmics);
    Ok(())
}

/// The S3 provider of `cos://` addresses that nf-tencentcloud's `tencentcloud` scope names:
/// Tencent Cloud Object Storage speaks the S3 API at `cos.<region>.myqcloud.com`.
pub(crate) fn cos(config: &Config) -> Option<ProviderSettings> {
    let scope = Scope(config);
    let key = |name: &'static str| ["tencentcloud", name];
    let region = scope.text(&key("region"))?;
    let secret = |name| {
        scope
            .text(&key(name))
            .map(|value| Secret::written(value, origin()))
    };
    let settings = S3 {
        endpoint: Some(format!("https://cos.{region}.myqcloud.com")),
        region: Some(region),
        path_style: Some(false),
        access_key_id: secret("secretId"),
        secret_access_key: secret("secretKey"),
        session_token: secret("sessionToken"),
        ..S3::default()
    };
    Some(ProviderSettings {
        name: "tencentcloud".into(),
        prefixes: vec!["cos://".into()],
        kind: Kind::S3(settings),
        origin: origin(),
    })
}

/// Whether `process.executor` or the executor of a process selector is `name`.
fn names_executor(config: &Config, name: &str) -> bool {
    let named = |value: Option<Value>| value.as_ref().and_then(text).as_deref() == Some(name);
    if named(config.get(&["process", "executor"])) {
        return true;
    }
    let Some(Value::Map(scope)) = config.get(&["process"]) else {
        return false;
    };
    let scope = scope.borrow();
    scope.entries.iter().any(|(key, body)| {
        let selector = key.0.to_string();
        let Value::Map(body) = body else {
            return false;
        };
        let body = body.borrow();
        let executor = (body.entries.iter())
            .find(|(setting, _)| setting.0.to_string() == "executor")
            .map(|(_, value)| value.clone());
        (selector.starts_with("withName:") || selector.starts_with("withLabel:")) && named(executor)
    })
}

/// Turns on the memory mode of the SLURM executor for nf-slurm-no-memory's executor
/// `slurm_no_memory`, with its memory per core (4 GB by default, as the plugin's) and account.
pub(crate) fn slurm(config: &Config, executors: &mut crate::setup::Executors) {
    if !names_executor(config, "slurm_no_memory") {
        return;
    }
    let scope = Scope(config);
    let key = |name: &'static str| ["executor", "$slurm_no_memory", name];
    if executors.slurm_cpu_memory.is_none() {
        let gb = scope.text(&key("memoryPerCore"));
        let gb = gb.and_then(|text| text.trim().parse::<f64>().ok());
        executors.slurm_cpu_memory = Some(gb.filter(|gb| *gb > 0.0).unwrap_or(4.0));
    }
    if let Some(account) = scope.text(&key("account")) {
        let options = executors.slurm_options.get_or_insert_with(Vec::new);
        let named = |option: &String| option == "-A" || option.starts_with("--account");
        if !options.iter().any(named) {
            options.push(format!("--account={account}"));
        }
    }
}

/// Warns once for each setting of nf-ospool's `executor.$ospool` that uwe's HTCondor executor
/// does without, as it keeps its submit files and stages the files of each job itself.
pub(crate) fn ospool(config: &Config) {
    let scope = Scope(config);
    let staged = "uwe stages the files of each job itself";
    for (name, why) in [
        (
            "submitFileDir",
            "uwe keeps each submit file in the folder of its attempt",
        ),
        (
            "sharedFilesystem",
            "`staging` of [executor.htcondor] tells how jobs get files",
        ),
        ("pathMappings", staged),
        ("autoStageDirectories", staged),
        ("stageBinDir", staged),
        ("unstageOutputs", staged),
        (
            "getenv",
            "uwe gives the jobs only the variables that they need",
        ),
    ] {
        if scope.get(&["executor", "$ospool", name]).is_some() {
            tracing::warn!("uwe ignores executor.$ospool.{name}: {why}");
        }
    }
}

/// Fills the Flyte settings that `[executor.flyte]` leaves unset from nf-flyte's `flyte` scope,
/// and the staging root from an object-store work dir.
pub(crate) fn flyte(config: &Config, flyte: &mut crate::flyte::Config) {
    let scope = Scope(config);
    let settings = &mut flyte.settings;
    for (slot, key) in [
        (&mut settings.endpoint, "endpoint"),
        (&mut settings.org, "org"),
        (&mut settings.project, "project"),
        (&mut settings.domain, "domain"),
        (&mut settings.queue, "queue"),
    ] {
        if slot.is_none() {
            *slot = scope.text(&["flyte", key]);
        }
    }
    let poll = scope.duration(&["flyte", "pollInterval"]);
    if let Some(poll) = poll.filter(|poll| poll.as_secs() >= 1) {
        settings.poll = poll;
    }
    let joined = "uwe makes one Flyte run for each task attempt";
    for (key, why) in [
        ("runName", joined),
        ("parentAction", joined),
        ("runOutputBase", joined),
        ("heartbeatTimeout", joined),
        ("rootImage", joined),
        ("cache", "uwe keeps its own cache of task results"),
        (
            "authCommand",
            "uwe signs in with the API key of FLYTE_API_KEY or api_key",
        ),
    ] {
        if scope.get(&["flyte", key]).is_some() {
            tracing::warn!("uwe ignores flyte.{key}: {why}");
        }
    }
    object_work(&scope, &mut flyte.staging);
}

/// Fills the Snowflake settings that `[executor.snowflake]` leaves unset from nf-snowflake's
/// `snowflake` scope, where the variable `computePool` wins as in the plugin, and the staging
/// root from an object-store work dir.
pub(crate) fn snowflake(
    config: &Config,
    snowflake: &mut crate::snowflake::Config,
) -> Result<(), String> {
    let scope = Scope(config);
    let settings = &mut snowflake.settings;
    if settings.compute_pool.is_none() {
        let variable = std::env::var("computePool")
            .ok()
            .filter(|pool| !pool.is_empty());
        let pool = variable.or_else(|| scope.text(&["snowflake", "computePool"]));
        if let Some(pool) = pool
            .as_deref()
            .filter(|pool| !snowflake_executor::identifier(pool))
        {
            return Err(format!(
                "snowflake.computePool: `{pool}` is no Snowflake identifier"
            ));
        }
        settings.compute_pool = pool;
    }
    if settings.mappings.is_empty()
        && let Some(text) = scope.text(&["snowflake", "registryMappings"])
    {
        settings.mappings = snowflake_executor::mappings(&text)
            .map_err(|message| format!("snowflake.registryMappings {message}"))?;
    }
    if settings.connection.is_none() {
        settings.connection = scope.text(&["snowflake", "connectionName"]);
    }
    if scope
        .text(&["workDir"])
        .is_some_and(|work| work.starts_with("snowflake://"))
    {
        tracing::warn!(
            "uwe does not use a snowflake:// work dir: its jobs stage through the staging root \
             of [executor.snowflake], or an s3://, gs:// or az:// workDir"
        );
    }
    object_work(&scope, &mut snowflake.staging);
    Ok(())
}

/// Makes an object-store work dir the staging root of an executor whose config file sets none.
fn object_work(scope: &Scope<'_>, staging: &mut crate::staging::Settings) {
    let objects = ["s3://", "gs://", "az://"];
    let work = scope.text(&["workDir"]);
    let work = work.filter(|work| objects.iter().any(|scheme| work.starts_with(scheme)));
    if staging.root.is_none()
        && let Some(work) = work
    {
        staging.root = Some(work);
        staging.mode = crate::staging::Mode::ObjectStore;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that prints nothing.
    struct Quiet;

    impl nextflow_evaluator::interpreter::Host for Quiet {
        fn print(&mut self, _: &str) {}
    }

    /// The config of a pipeline whose `nextflow.config` is `text`.
    pub(crate) fn loaded(text: &str) -> Config {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("nextflow.config");
        std::fs::write(&file, text).expect("writes the config");
        let options = nextflow_config::Options {
            profiles: None,
            params: Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        nextflow_config::load(&[file], &options, &mut Quiet).expect("a config")
    }

    #[test]
    fn maps_nomad_scope() {
        let text = "nomad {\n client {\n  address = 'http://127.0.0.1:4646'\n  token = 't'\n  \
                    pollInterval = '5s'\n }\n jobs {\n  datacenters = ['dc1', 'dc2']\n  \
                    namespace = 'lab'\n  cleanup = 'never'\n  cpuMode = 'cpu'\n  \
                    meta = [owner: 'team-x']\n  \
                    spreads = [[name: 'node.datacenter', weight: 50]]\n  \
                    volumes = [[type: 'csi', name: 'work', path: '/work', workDir: true]]\n  \
                    failOnPlacementFailure = true\n  placementFailureTimeout = '5m'\n }\n \
                    s5cmd {\n  enabled = true\n  workDir {\n   enabled = true\n   \
                    bucket = 's3://runs'\n   prefix = 'nomad/'\n  }\n  s3 {\n   \
                    endpoint = 'http://127.0.0.1:9000'\n   usePathStyle = true\n  }\n }\n}\n";
        let mut nomad = crate::nomad::Config::default();
        nomad.settings.namespace = Some("mine".into());
        let provider = super::nomad(&loaded(text), &mut nomad).expect("a mapping");
        let settings = &nomad.settings;
        assert_eq!(settings.address.as_deref(), Some("http://127.0.0.1:4646"));
        assert!(settings.token.is_some());
        assert_eq!(
            settings.namespace.as_deref(),
            Some("mine"),
            "uwe's own setting wins"
        );
        assert_eq!(settings.datacenters, ["dc1", "dc2"]);
        assert_eq!(settings.cleanup, nomad_executor::Cleanup::Never);
        assert_eq!(settings.cpu_mode, nomad_executor::CpuMode::Megahertz);
        assert_eq!(settings.meta["owner"], "team-x");
        assert_eq!(settings.spreads[0]["Attribute"], "${node.datacenter}");
        assert_eq!(settings.volumes[0].kind, "csi");
        assert!(settings.fail_unplaced);
        assert_eq!(settings.placement, Duration::from_secs(300));
        assert_eq!(settings.poll, Duration::from_secs(5));
        assert_eq!(nomad.staging.root.as_deref(), Some("s3://runs/nomad"));
        assert_eq!(nomad.staging.mode, crate::staging::Mode::ObjectStore);
        let provider = provider.expect("a provider of the bucket");
        assert_eq!(provider.prefixes, ["s3://runs/"]);
        let Kind::S3(s3) = provider.kind else {
            panic!("an S3 provider");
        };
        assert_eq!(s3.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(s3.path_style, Some(true));
        let refused = loaded("nomad.jobs.privileged = true\n");
        let error = super::nomad(&refused, &mut crate::nomad::Config::default());
        assert!(error.is_err_and(|error| error.contains("never starts privileged")));
    }

    #[test]
    fn maps_kubernetes_scope() {
        let text = "process.executor = 'k8s'\nk8s {\n namespace = 'lab'\n context = 'gpu'\n \
                    serviceAccount = 'runner'\n storageClaimName = 'work'\n \
                    storageMountPath = '/data'\n imagePullPolicy = 'Always'\n \
                    pod = [[nodeSelector: 'disktype=ssd'], [label: 'tier', value: 'batch']]\n}\n";
        let mut executors = crate::setup::Executors::default();
        super::kubernetes(&loaded(text), &mut executors).expect("a mapping");
        assert_eq!(executors.kubernetes_namespace.as_deref(), Some("lab"));
        assert_eq!(executors.kubernetes_context.as_deref(), Some("gpu"));
        assert_eq!(
            executors.kubernetes_pod.service_account.as_deref(),
            Some("runner")
        );
        let kubernetes_executor::Volumes::Under { root, source } = &executors.kubernetes_volumes
        else {
            panic!("a shared claim");
        };
        assert_eq!(root, Path::new("/data"));
        assert_eq!(source["persistentVolumeClaim"]["claimName"], "work");
        let refused = loaded("k8s.pod = [privileged: true]\n");
        let error = super::kubernetes(&refused, &mut crate::setup::Executors::default());
        assert!(error.is_err_and(|error| error.contains("never starts privileged")));
    }

    #[test]
    fn maps_tes_scope() {
        let text = "process.executor = 'tes'\ntes {\n endpoint = 'http://127.0.0.1:8000/'\n \
                    oauthToken = 't'\n apiKey = 'k'\n apiKeyParamName = 'key'\n \
                    apiKeyParamMode = 'header'\n timeout = 30\n \
                    tags { project = 'demo'; owner = 'lab' }\n}\n";
        let mut executors = crate::setup::Executors::default();
        super::tes(&loaded(text), &mut executors).expect("a mapping");
        assert_eq!(
            executors.tes_server.as_deref(),
            Some("http://127.0.0.1:8000/v1")
        );
        assert!(executors.tes_token.is_some());
        let settings = &executors.tes;
        assert_eq!(settings.api_key_name.as_deref(), Some("key"));
        assert!(settings.api_key_header);
        assert_eq!(settings.timeout, Some(Duration::from_secs(30)));
        assert_eq!(settings.tags["project"], "demo");
        assert_eq!(settings.platform, None);
        let text = "process.executor = 'tencentcloud-omics'\ntencentcloud {\n \
                    region = 'ap-guangzhou'\n secretId = 'id'\n secretKey = 'key'\n \
                    omics {\n  endpoint = 'http://127.0.0.1:1/'\n  accessToken = 't'\n }\n}\n";
        let config = loaded(text);
        let mut executors = crate::setup::Executors::default();
        super::tes(&config, &mut executors).expect("a mapping");
        assert_eq!(
            executors.tes_server.as_deref(),
            Some("http://127.0.0.1:1/v1")
        );
        assert_eq!(
            executors.tes.platform,
            Some(tes_executor::Platform::TencentOmics)
        );
        let cos = super::cos(&config).expect("a provider");
        assert_eq!(cos.prefixes, ["cos://"]);
        let Kind::S3(s3) = cos.kind else {
            panic!("an S3 provider");
        };
        assert_eq!(
            s3.endpoint.as_deref(),
            Some("https://cos.ap-guangzhou.myqcloud.com")
        );
        let missing = loaded("process.executor = 'tencentcloud-omics'\n");
        let error = super::tes(&missing, &mut crate::setup::Executors::default());
        assert_eq!(
            error,
            Err("tencentcloud.omics.endpoint must be specified!".into())
        );
    }

    #[test]
    fn maps_slurm_memory() {
        let text = "process.executor = 'slurm_no_memory'\nexecutor {\n $slurm_no_memory {\n  \
                    account = 'lab01'\n }\n}\n";
        let mut executors = crate::setup::Executors::default();
        super::slurm(&loaded(text), &mut executors);
        assert_eq!(executors.slurm_cpu_memory, Some(4.0));
        assert_eq!(
            executors.slurm_options,
            Some(vec!["--account=lab01".to_owned()])
        );
        let text = "process {\n withLabel: big { executor = 'slurm_no_memory' }\n}\n\
                    executor.'$slurm_no_memory'.memoryPerCore = '8'\n";
        let mut executors = crate::setup::Executors::default();
        super::slurm(&loaded(text), &mut executors);
        assert_eq!(executors.slurm_cpu_memory, Some(8.0));
        let mut executors = crate::setup::Executors::default();
        super::slurm(&loaded("process.executor = 'slurm'\n"), &mut executors);
        assert_eq!(executors.slurm_cpu_memory, None);
    }

    #[test]
    fn maps_flyte_scope() {
        let text = "workDir = 's3://runs/work'\nflyte {\n endpoint = 'flyte.example.org'\n \
                    project = 'flows'\n domain = 'development'\n queue = 'gpu'\n \
                    pollInterval = '2s'\n runName = 'r1'\n}\n";
        let mut flyte = crate::flyte::Config::default();
        flyte.settings.project = Some("mine".into());
        super::flyte(&loaded(text), &mut flyte);
        let settings = &flyte.settings;
        assert_eq!(settings.endpoint.as_deref(), Some("flyte.example.org"));
        assert_eq!(
            settings.project.as_deref(),
            Some("mine"),
            "uwe's own setting wins"
        );
        assert_eq!(settings.domain.as_deref(), Some("development"));
        assert_eq!(settings.queue.as_deref(), Some("gpu"));
        assert_eq!(settings.poll, Duration::from_secs(2));
        assert_eq!(flyte.staging.root.as_deref(), Some("s3://runs/work"));
        assert_eq!(flyte.staging.mode, crate::staging::Mode::ObjectStore);
    }

    #[test]
    fn maps_snowflake_scope() {
        let text = "workDir = 'gs://runs/work'\nsnowflake {\n computePool = 'TASKS'\n \
                    registryMappings = 'docker.io:/lab/images/repo'\n connectionName = 'lab'\n}\n";
        let mut snowflake = crate::snowflake::Config::default();
        super::snowflake(&loaded(text), &mut snowflake).expect("a mapping");
        let settings = &snowflake.settings;
        assert_eq!(settings.compute_pool.as_deref(), Some("TASKS"));
        assert_eq!(settings.mappings[0].1, "/lab/images/repo");
        assert_eq!(settings.connection.as_deref(), Some("lab"));
        assert_eq!(snowflake.staging.root.as_deref(), Some("gs://runs/work"));
        let wrong = loaded("snowflake.computePool = 'a; drop'\n");
        let error = super::snowflake(&wrong, &mut crate::snowflake::Config::default());
        assert!(error.is_err_and(|message| message.contains("computePool")));
    }
}
