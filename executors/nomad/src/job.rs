//! The Nomad batch job of one attempt: its id, the task that runs the command in a container,
//! its resources, placement rules, volumes, restart policies and the helper artifact.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use executor::process::Mount;
use serde_json::{Map, Value, json};
use task_model::Placement;

use crate::options::Options;
use crate::settings::{CpuMode, Settings, Volume};

/// The names of the task group and of the task in each job.
pub const GROUP: &str = "uwe";
pub const TASK: &str = "task";

/// The memory of a task that asks for none, in MiB, as nf-nomad gives it.
const MEMORY_MB: u64 = 1024;

/// The longest id that Nomad takes.
const LONGEST_ID: usize = 128;

/// One attempt as Nomad runs it.
#[derive(Debug)]
pub struct Shape<'a> {
    pub id: &'a str,
    /// The name of the attempt, which goes into `Meta`.
    pub attempt: &'a str,
    pub image: &'a str,
    /// The command of the container: its first word is the docker `command`, the others `args`.
    pub command: Vec<String>,
    /// Where the container starts, and the user it runs as, like `1000:100`.
    pub work_dir: Option<String>,
    pub user: Option<String>,
    /// Host folders that the container sees, as bind mounts of the driver.
    pub mounts: Vec<Mount>,
    /// The helper that Nomad downloads into the task folder before the task starts.
    pub artifact: Option<Artifact<'a>>,
    pub placement: &'a Placement,
}

/// A file that Nomad downloads and checks before the task starts.
#[derive(Debug)]
pub struct Artifact<'a> {
    pub url: &'a str,
    pub sha256: &'a str,
    /// The path in the task folder, like `local/uwe`.
    pub path: &'a str,
}

/// The id of the job of the attempt `name`: `uwe-`, the name in lower case with each run of
/// other characters than letters, digits, `.` and `_` as one `-`. A name too long for Nomad keeps
/// its start and gets a digest of the whole name, so that ids stay unique.
pub fn job_id(name: &str) -> String {
    let mut clean = String::from("uwe-");
    for character in name.chars().map(|character| character.to_ascii_lowercase()) {
        match character {
            'a'..='z' | '0'..='9' | '.' | '_' => clean.push(character),
            _ if !clean.ends_with('-') => clean.push('-'),
            _ => {}
        }
    }
    let mut clean = clean.trim_end_matches('-').to_owned();
    if clean.len() > LONGEST_ID {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(name.as_bytes());
        let short: String = digest[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        clean.truncate(LONGEST_ID - short.len() - 1);
        clean = format!("{}-{short}", clean.trim_end_matches('-'));
    }
    clean
}

/// The body of the job, with the settings and the `nomadOptions` of the task, which win.
pub fn body(shape: &Shape<'_>, settings: &Settings) -> Result<Value, String> {
    let options = Options::of(shape.placement)?;
    let placement = shape.placement;
    let mut job = json!({"ID": shape.id, "Name": shape.id, "Type": "batch"});
    let mut datacenters = settings.datacenters.clone();
    for datacenter in &options.datacenters {
        if !datacenters.contains(datacenter) {
            datacenters.push(datacenter.clone());
        }
    }
    if !datacenters.is_empty() {
        job["Datacenters"] = json!(datacenters);
    }
    if let Some(namespace) = &settings.namespace {
        job["Namespace"] = json!(namespace);
    }
    if let Some(region) = &settings.region {
        job["Region"] = json!(region);
    }
    if let Some(pool) = options.node_pool.as_ref().or(settings.node_pool.as_ref()) {
        job["NodePool"] = json!(pool);
    }
    if let Some(priority) = options.priority.or(settings.priority) {
        job["Priority"] = json!(priority);
    }
    let mut meta = settings.meta.clone();
    meta.extend(options.meta.clone());
    meta.insert("uwe_attempt".into(), shape.attempt.to_owned());
    job["Meta"] = json!(meta);
    for (key, global, own) in [
        ("Constraints", &settings.constraints, &options.constraints),
        ("Affinities", &settings.affinities, &options.affinities),
        ("Spreads", &settings.spreads, &options.spreads),
    ] {
        let all: Vec<&Value> = global.iter().chain(own).collect();
        if !all.is_empty() {
            job[key] = json!(all);
        }
    }
    let driver = options.driver.as_deref().unwrap_or(&settings.driver);
    let mut task = json!({
        "Name": TASK,
        "Driver": driver,
        "Config": config(shape, settings, driver),
        "Resources": resources(placement, settings, &options)?,
    });
    if let Some(user) = &shape.user {
        task["User"] = json!(user);
    }
    if let Some(artifact) = &shape.artifact {
        task["Artifacts"] = json!([{
            "GetterSource": artifact.url,
            "GetterMode": "file",
            "GetterOptions": {"checksum": format!("sha256:{}", artifact.sha256)},
            "RelativeDest": artifact.path,
        }]);
    }
    if let Some(delay) = options.shutdown_delay {
        task["ShutdownDelay"] = json!(delay);
    }
    let volumes: Vec<&Volume> = settings.volumes.iter().chain(&options.volumes).collect();
    let mut requests = Map::new();
    let mut mounts = Vec::new();
    for (index, volume) in volumes.iter().enumerate() {
        let name = format!("vol_{}", index + 1);
        let mut request = json!({
            "Name": name,
            "Type": volume.kind,
            "Source": volume.name,
            "ReadOnly": volume.read_only,
        });
        if volume.kind == "csi" {
            request["AccessMode"] = json!("multi-node-multi-writer");
            request["AttachmentMode"] = json!("file-system");
            if volume.read_only {
                request["AccessMode"] = json!("multi-node-reader-only");
            }
        }
        requests.insert(name.clone(), request);
        mounts.push(json!({
            "Volume": name,
            "Destination": volume.path,
            "ReadOnly": volume.read_only,
        }));
    }
    if !mounts.is_empty() {
        task["VolumeMounts"] = json!(mounts);
    }
    let mut restart = json!({"Attempts": options.restart.attempts.unwrap_or(settings.restart_attempts), "Mode": "fail"});
    options.restart.fill(&mut restart);
    let mut reschedule = json!({
        "Attempts": options.reschedule.attempts.unwrap_or(settings.reschedule_attempts),
        "Unlimited": false,
    });
    options.reschedule.fill(&mut reschedule);
    let mut group = json!({
        "Name": GROUP,
        "Count": 1,
        "RestartPolicy": restart,
        "ReschedulePolicy": reschedule,
        "Tasks": [task],
    });
    if !requests.is_empty() {
        group["Volumes"] = Value::Object(requests);
    }
    job["TaskGroups"] = json!([group]);
    Ok(job)
}

/// The driver config of the task: image, command, start folder, mounts, and no privileges.
fn config(shape: &Shape<'_>, settings: &Settings, driver: &str) -> Value {
    let mut config = json!({"image": shape.image, "privileged": false});
    if let Some((first, rest)) = shape.command.split_first() {
        config["command"] = json!(first);
        config["args"] = json!(rest);
    }
    let start = if driver == "podman" {
        "working_dir"
    } else {
        "work_dir"
    };
    if let Some(folder) = &shape.work_dir {
        config[start] = json!(folder);
    }
    if let Some(mode) = &settings.network_mode {
        config["network_mode"] = json!(mode);
    }
    if shape.placement.offline {
        config["network_mode"] = json!("none");
    }
    config["security_opt"] = json!(["no-new-privileges"]);
    if shape.mounts.is_empty() {
        return config;
    }
    match driver {
        "podman" => {
            let binds: Vec<String> = (shape.mounts.iter())
                .map(|mount| {
                    let mode = if mount.read_only { ":ro" } else { "" };
                    format!("{}:{}{mode}", mount.source.display(), mount.target)
                })
                .collect();
            config["volumes"] = json!(binds);
        }
        _ => {
            let binds: Vec<Value> = (shape.mounts.iter())
                .map(|mount| {
                    json!({
                        "type": "bind",
                        "source": mount.source.to_string_lossy(),
                        "target": mount.target,
                        "readonly": mount.read_only,
                    })
                })
                .collect();
            config["mount"] = json!(binds);
        }
    }
    config
}

/// The CPU, memory and devices of the task.
fn resources(
    placement: &Placement,
    settings: &Settings,
    options: &Options,
) -> Result<Value, String> {
    let cpus = u64::from(placement.cpus.max(1));
    let memory = (placement.memory_bytes.map(|bytes| bytes.div_ceil(1 << 20))).unwrap_or(MEMORY_MB);
    let mut resources =
        json!({"MemoryMB": memory, "MemoryMaxMB": options.memory_max.unwrap_or(memory)});
    match (options.cores, options.cpu, settings.cpu_mode) {
        (Some(cores), _, _) => resources["Cores"] = json!(cores),
        (_, Some(megahertz), _) => resources["CPU"] = json!(megahertz),
        (_, _, CpuMode::Cores) => resources["Cores"] = json!(cpus),
        (_, _, CpuMode::Megahertz) => resources["CPU"] = json!(cpus * 1000),
    }
    let mut devices: BTreeMap<String, u64> = BTreeMap::new();
    for gpu in placement.accelerators.iter().filter(|gpu| gpu.count > 0) {
        let name = match gpu.kind.as_deref() {
            Some(kind) if kind.contains('/') => kind.to_owned(),
            _ => settings.device.clone(),
        };
        *devices.entry(name).or_default() += u64::from(gpu.count);
    }
    let devices: Vec<Value> = match &options.device {
        Some(device) => vec![device.clone()],
        None => (devices.into_iter())
            .map(|(name, count)| json!({"Name": name, "Count": count}))
            .collect(),
    };
    if !devices.is_empty() {
        resources["Devices"] = json!(devices);
    }
    Ok(resources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_jobs() {
        assert_eq!(job_id("run-ALIGN (1)-2"), "uwe-run-align-1-2");
        let long = "x".repeat(200);
        let id = job_id(&long);
        assert_eq!(id.len(), LONGEST_ID);
        assert_ne!(id, job_id(&format!("{long}y")));
    }

    #[test]
    fn cuts_offline_tasks() {
        let placement = Placement {
            offline: true,
            ..Placement::default()
        };
        let shape = Shape {
            id: "uwe-a",
            attempt: "a",
            image: "ubuntu",
            command: vec!["sh".into(), "-c".into(), "true".into()],
            work_dir: None,
            user: None,
            mounts: Vec::new(),
            artifact: None,
            placement: &placement,
        };
        let settings = Settings {
            network_mode: Some("bridge".into()),
            ..Settings::default()
        };
        let job = body(&shape, &settings).expect("a body");
        let config = &job["TaskGroups"][0]["Tasks"][0]["Config"];
        assert_eq!(config["network_mode"], "none");
        assert_eq!(
            (config["command"].as_str(), config["privileged"].as_bool()),
            (Some("sh"), Some(false))
        );
    }
}
