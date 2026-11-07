//! Reads the settings of the Nomad executor from `[executor.nomad]`: the agent and its token,
//! where jobs run, their placement rules, volumes, cleanup, polling and the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::time::Duration;

use config_file::{ConfigError, Secret, Table};
use executor::batch::{Helper, TASK_FOLDER, helper};
use serde_json::Value;

/// The address of the Nomad agent when neither the settings nor `NOMAD_ADDR` name one, as the
/// Nomad command line has it.
pub const ADDRESS: &str = "http://127.0.0.1:4646";

/// The device of a GPU that names no device of its own, as nf-nomad asks for it.
pub const DEVICE: &str = "nvidia/gpu";

/// How often Nomad is asked about a job, by default, as nf-nomad does.
const POLL: Duration = Duration::from_secs(1);

/// How long a job may find no node before it fails, by default, as nf-nomad's
/// `placementFailureTimeout`.
const PLACEMENT: Duration = Duration::from_secs(30 * 60);

/// A token from the config file of uwe or from the Nextflow config, which `Debug` never shows.
#[derive(Clone, PartialEq, Eq)]
pub enum Hidden {
    Secret(Secret),
    Text(String),
}

impl Hidden {
    pub fn reveal(&self) -> Result<String, String> {
        match self {
            Hidden::Secret(secret) => secret.reveal().map_err(|error| error.to_string()),
            Hidden::Text(text) => Ok(text.clone()),
        }
    }
}

impl std::fmt::Debug for Hidden {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[hidden]")
    }
}

/// How the CPUs of a task go to Nomad: as whole cores, or as MHz of 1000 per CPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuMode {
    #[default]
    Cores,
    Megahertz,
}

/// Which finished jobs uwe purges from Nomad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cleanup {
    Always,
    Never,
    /// Only successful jobs, so that failed ones stay for a look, as Nextflow's `k8s.cleanup`.
    #[default]
    OnSuccess,
}

/// A host or CSI volume of the Nomad clients, mounted in each task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// `host` or `csi`.
    pub kind: String,
    /// The name of the volume in Nomad.
    pub name: String,
    /// Where the containers see it.
    pub path: String,
    pub read_only: bool,
}

/// The settings of the executor. The Nextflow `nomad` scope fills those that the config file
/// of uwe leaves unset.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The address of the Nomad agent; else `NOMAD_ADDR`, else [`ADDRESS`].
    pub address: Option<String>,
    /// The ACL token, sent as `X-Nomad-Token`; else `NOMAD_TOKEN`.
    pub token: Option<Hidden>,
    pub namespace: Option<String>,
    pub region: Option<String>,
    pub datacenters: Vec<String>,
    pub node_pool: Option<String>,
    /// The task driver: `docker` or `podman`.
    pub driver: String,
    /// The network mode of the containers, like `bridge`.
    pub network_mode: Option<String>,
    pub cpu_mode: CpuMode,
    /// The device of GPUs that name no device of their own, like `nvidia/gpu`.
    pub device: String,
    /// The priority of the jobs, from 1 to 100; `None` for Nomad's default.
    pub priority: Option<u32>,
    /// More `Meta` of the jobs.
    pub meta: BTreeMap<String, String>,
    /// Nomad `Constraints`, `Affinities` and `Spreads` of every job, in the shape of the API.
    pub constraints: Vec<Value>,
    pub affinities: Vec<Value>,
    pub spreads: Vec<Value>,
    pub volumes: Vec<Volume>,
    /// How often Nomad restarts a failed task on its node, and places it on another node.
    pub restart_attempts: u32,
    pub reschedule_attempts: u32,
    pub cleanup: Cleanup,
    /// Whether a job that no node can take fails at once, rather than after `placement`.
    pub fail_unplaced: bool,
    pub placement: Duration,
    pub poll: Duration,
    /// The folder of the container that holds the folders of attempts, for object-store
    /// staging.
    pub task_folder: String,
    /// Where the jobs get the static uwe binary; `None` for one that uwe uploads.
    pub helper: Option<Helper>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            address: None,
            token: None,
            namespace: None,
            region: None,
            datacenters: Vec::new(),
            node_pool: None,
            driver: "docker".into(),
            network_mode: None,
            cpu_mode: CpuMode::Cores,
            device: DEVICE.into(),
            priority: None,
            meta: BTreeMap::new(),
            constraints: Vec::new(),
            affinities: Vec::new(),
            spreads: Vec::new(),
            volumes: Vec::new(),
            restart_attempts: 0,
            reschedule_attempts: 1,
            cleanup: Cleanup::OnSuccess,
            fail_unplaced: false,
            placement: PLACEMENT,
            poll: POLL,
            task_folder: TASK_FOLDER.into(),
            helper: None,
        }
    }
}

impl Settings {
    pub const KEYS: &'static [&'static str] = &[
        "address",
        "token",
        "namespace",
        "region",
        "datacenters",
        "node_pool",
        "driver",
        "network_mode",
        "cpu_mode",
        "device",
        "priority",
        "meta",
        "constraints",
        "affinities",
        "spreads",
        "volumes",
        "restart_attempts",
        "reschedule_attempts",
        "cleanup",
        "fail_unplaced",
        "placement_minutes",
        "poll_seconds",
        "task_folder",
        "helper_path",
        "helper_url",
        "helper_sha256",
    ];

    /// The settings of `[executor.nomad]`.
    pub fn read(table: &Table<'_>) -> Result<Settings, ConfigError> {
        let mut settings = Settings {
            address: table.text("address")?,
            token: table.secret("token")?.map(Hidden::Secret),
            namespace: table.text("namespace")?,
            region: table.text("region")?,
            datacenters: table.texts("datacenters")?.unwrap_or_default(),
            node_pool: table.text("node_pool")?,
            network_mode: table.text("network_mode")?,
            fail_unplaced: table.flag("fail_unplaced")?.unwrap_or(false),
            helper: helper(table)?,
            ..Settings::default()
        };
        let invalid = |key: &str, message: String| table.invalid(key, message);
        if let Some(address) = &settings.address
            && !address.starts_with("http://")
            && !address.starts_with("https://")
        {
            return Err(table.invalid("address", "must be an http:// or https:// address"));
        }
        if let Some(driver) = table.text("driver")? {
            settings.driver = driver_name(&driver).map_err(|message| invalid("driver", message))?;
        }
        if let Some(mode) = table.text("cpu_mode")? {
            settings.cpu_mode = cpu_mode(&mode).map_err(|message| invalid("cpu_mode", message))?;
        }
        if let Some(device) = table.text("device")? {
            settings.device = device;
        }
        if let Some(priority) = table.integer("priority")? {
            let priority = u32::try_from(priority).ok().filter(|value| *value >= 1);
            settings.priority = Some(
                priority
                    .filter(|value| *value <= 100)
                    .ok_or_else(|| table.invalid("priority", "must be between 1 and 100"))?,
            );
        }
        if let Some(meta) = table.table("meta")? {
            for key in meta.keys() {
                settings
                    .meta
                    .insert(key.to_owned(), meta.text(key)?.unwrap_or_default());
            }
        }
        for (key, slot, read) in [
            ("constraints", &mut settings.constraints, constraint as Rule),
            ("affinities", &mut settings.affinities, affinity),
            ("spreads", &mut settings.spreads, spread),
        ] {
            *slot = rules(table.json(key)?, read).map_err(|message| invalid(key, message))?;
        }
        if let Some(volumes) = table.json("volumes")? {
            settings.volumes =
                volume_list(&volumes).map_err(|message| invalid("volumes", message))?;
        }
        let count = |key: &str| -> Result<Option<u32>, ConfigError> {
            match table.integer(key)? {
                None => Ok(None),
                Some(count) => (u32::try_from(count).ok().filter(|count| *count <= 100))
                    .map(Some)
                    .ok_or_else(|| table.invalid(key, "must be between 0 and 100")),
            }
        };
        if let Some(count) = count("restart_attempts")? {
            settings.restart_attempts = count;
        }
        if let Some(count) = count("reschedule_attempts")? {
            settings.reschedule_attempts = count;
        }
        if let Some(cleanup) = table.text("cleanup")? {
            settings.cleanup =
                cleanup_mode(&cleanup).map_err(|message| invalid("cleanup", message))?;
        }
        if let Some(minutes) = table.integer("placement_minutes")? {
            if !(1..=10_080).contains(&minutes) {
                return Err(table.invalid("placement_minutes", "must be between 1 and 10080"));
            }
            settings.placement = Duration::from_secs(minutes as u64 * 60);
        }
        if let Some(seconds) = table.integer("poll_seconds")? {
            if !(1..=3600).contains(&seconds) {
                return Err(table.invalid("poll_seconds", "must be between 1 and 3600"));
            }
            settings.poll = Duration::from_secs(seconds as u64);
        }
        if let Some(folder) = table.text("task_folder")? {
            if !folder.starts_with('/') || folder.split('/').any(|part| part == "..") {
                return Err(table.invalid("task_folder", "must be an absolute folder"));
            }
            settings.task_folder = folder;
        }
        Ok(settings)
    }
}

/// Reads one rule of the shape that uwe's settings and Nextflow's `nomadOptions` share.
pub type Rule = fn(&serde_json::Map<String, Value>) -> Result<Value, String>;

/// The rules of a list, each read by `read`; none without a list.
pub fn rules(list: Option<Value>, read: Rule) -> Result<Vec<Value>, String> {
    let items = match list {
        None => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(Value::Object(item)) => vec![Value::Object(item)],
        Some(_) => return Err("must be a list of tables".into()),
    };
    (items.iter())
        .map(|item| match item {
            Value::Object(item) => read(item),
            _ => Err("must be a list of tables".into()),
        })
        .collect()
}

/// A Nomad constraint from `attribute`, `operator` (default `=`) and `value`.
pub fn constraint(item: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let attribute = word(item, "attribute")?.ok_or("needs an attribute, like ${node.class}")?;
    let operator = word(item, "operator")?.unwrap_or_else(|| "=".into());
    let value = word(item, "value")?.unwrap_or_default();
    Ok(serde_json::json!({"LTarget": attribute, "Operand": operator, "RTarget": value}))
}

/// A Nomad affinity: a constraint with a `weight` from -100 to 100 (default 50).
pub fn affinity(item: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let mut rule = constraint(item)?;
    rule["Weight"] = serde_json::json!(weight(item)?.unwrap_or(50));
    Ok(rule)
}

/// A Nomad spread over an `attribute` (or Nextflow's `name`), with a `weight` and `targets`
/// that map values to percents.
pub fn spread(item: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let attribute = match (word(item, "attribute")?, word(item, "name")?) {
        (Some(attribute), _) => attribute,
        // Nextflow's spread names the attribute without `${...}`.
        (None, Some(name)) if name.starts_with("${") => name,
        (None, Some(name)) => format!("${{{name}}}"),
        (None, None) => return Err("needs an attribute, like ${node.datacenter}".into()),
    };
    let mut targets = Vec::new();
    if let Some(Value::Object(map)) = item.get("targets") {
        for (value, percent) in map {
            let percent = percent.as_u64().filter(|percent| *percent <= 100);
            let percent = percent.ok_or("needs targets with percents from 0 to 100")?;
            targets.push(serde_json::json!({"Value": value, "Percent": percent}));
        }
    }
    Ok(serde_json::json!({
        "Attribute": attribute,
        "Weight": weight(item)?.unwrap_or(50),
        "SpreadTarget": targets,
    }))
}

/// The volumes of a list of tables with `type`, `name`, `path` and `read_only`.
pub fn volume_list(list: &Value) -> Result<Vec<Volume>, String> {
    let items = list.as_array().ok_or("must be a list of tables")?;
    let mut volumes = Vec::new();
    for item in items {
        let item = item.as_object().ok_or("must be a list of tables")?;
        let kind = word(item, "type")?.unwrap_or_else(|| "host".into());
        if kind != "host" && kind != "csi" {
            return Err(format!("has the volume type `{kind}`; use host or csi"));
        }
        let name = word(item, "name")?.ok_or("needs the name of each volume in Nomad")?;
        let path = word(item, "path")?.ok_or("needs the path where tasks see each volume")?;
        if !path.starts_with('/') {
            return Err(format!("has the path `{path}`; use an absolute path"));
        }
        let read_only = match item.get("read_only").or(item.get("readOnly")) {
            None => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return Err("takes true or false for read_only".into()),
        };
        volumes.push(Volume {
            kind,
            name,
            path,
            read_only,
        });
    }
    Ok(volumes)
}

pub fn driver_name(name: &str) -> Result<String, String> {
    match name {
        "docker" | "podman" => Ok(name.to_owned()),
        _ => Err(format!(
            "is `{name}`; uwe runs Nomad tasks with the docker or podman driver"
        )),
    }
}

pub fn cpu_mode(name: &str) -> Result<CpuMode, String> {
    match name {
        "cores" => Ok(CpuMode::Cores),
        "cpu" => Ok(CpuMode::Megahertz),
        _ => Err(format!("is `{name}`; use cores or cpu")),
    }
}

pub fn cleanup_mode(name: &str) -> Result<Cleanup, String> {
    match name {
        "always" => Ok(Cleanup::Always),
        "never" => Ok(Cleanup::Never),
        "on-success" | "onSuccess" => Ok(Cleanup::OnSuccess),
        _ => Err(format!("is `{name}`; use always, never or on-success")),
    }
}

/// The text of `key`, also from a number or a flag.
fn word(item: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match item.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(value @ (Value::Number(_) | Value::Bool(_))) => Ok(Some(value.to_string())),
        Some(_) => Err(format!("takes text for {key}")),
    }
}

fn weight(item: &serde_json::Map<String, Value>) -> Result<Option<i64>, String> {
    match item.get("weight") {
        None => Ok(None),
        Some(weight) => (weight.as_i64())
            .filter(|weight| (-100..=100).contains(weight))
            .map(Some)
            .ok_or_else(|| "takes a weight from -100 to 100".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_file::ConfigFile;

    fn read(text: &str) -> Result<Settings, ConfigError> {
        let config = ConfigFile::parse(text, std::path::Path::new("uwe.toml"))?;
        let section = config.section("executor")?.expect("a section");
        Settings::read(&section.table("nomad")?.expect("a table"))
    }

    #[test]
    fn reads_nomad_settings() {
        let text = "[executor.nomad]\naddress = 'http://127.0.0.1:1'\nnamespace = 'lab'\n\
                    datacenters = ['dc1']\npriority = 80\ncpu_mode = 'cpu'\n\
                    meta = { owner = 'team-x' }\n\
                    constraints = [{ attribute = '${attr.cpu.arch}', value = 'amd64' }]\n\
                    spreads = [{ name = 'node.datacenter', weight = 100, targets = { dc1 = 70 } }]\n\
                    volumes = [{ type = 'csi', name = 'shared', path = '/data', read_only = true }]\n\
                    cleanup = 'never'\npoll_seconds = 5\nhelper_path = '/opt/uwe/uwe'\n";
        let settings = read(text).expect("settings");
        assert_eq!(settings.namespace.as_deref(), Some("lab"));
        assert_eq!(settings.datacenters, ["dc1"]);
        assert_eq!(settings.priority, Some(80));
        assert_eq!(settings.cpu_mode, CpuMode::Megahertz);
        assert_eq!(settings.meta["owner"], "team-x");
        let rule = serde_json::json!({"LTarget": "${attr.cpu.arch}", "Operand": "=",
                                      "RTarget": "amd64"});
        assert_eq!(settings.constraints, [rule]);
        assert_eq!(settings.spreads[0]["Attribute"], "${node.datacenter}");
        assert_eq!(settings.spreads[0]["SpreadTarget"][0]["Percent"], 70);
        assert_eq!(settings.volumes[0].kind, "csi");
        assert!(settings.volumes[0].read_only);
        assert_eq!(settings.cleanup, Cleanup::Never);
        assert_eq!(settings.poll, Duration::from_secs(5));
        assert_eq!(settings.helper, Some(Helper::Path("/opt/uwe/uwe".into())));
        assert_eq!(Settings::default().cleanup, Cleanup::OnSuccess);
        for wrong in [
            "address = '127.0.0.1:4646'",
            "driver = 'exec'",
            "priority = 101",
            "cpu_mode = 'mhz'",
            "constraints = [{ value = 'x' }]",
            "volumes = [{ type = 'nfs', name = 'a', path = '/a' }]",
            "volumes = [{ name = 'a', path = 'a' }]",
            "cleanup = 'sometimes'",
            "task_folder = 'tmp'",
        ] {
            let error = read(&format!("[executor.nomad]\n{wrong}\n"));
            assert!(error.is_err(), "{wrong}");
        }
    }
}
