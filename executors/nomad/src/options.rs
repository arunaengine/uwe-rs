//! Reads the `nomadOptions` of a task, as Nextflow's process config gives them: placement,
//! priority, meta, volumes, failure policies, resources and the driver, checked before submit.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use task_model::Placement;

use crate::settings::{Volume, affinity, constraint, rules, spread, volume_list};

/// The `nomadOptions` of a task, as Nextflow's process directive gives them.
#[derive(Debug, Default)]
pub struct Options {
    pub datacenters: Vec<String>,
    pub node_pool: Option<String>,
    pub priority: Option<u32>,
    pub meta: BTreeMap<String, String>,
    pub constraints: Vec<Value>,
    pub affinities: Vec<Value>,
    pub spreads: Vec<Value>,
    pub volumes: Vec<Volume>,
    pub shutdown_delay: Option<u64>,
    pub restart: Policy,
    pub reschedule: Policy,
    pub memory_max: Option<u64>,
    pub cpu: Option<u64>,
    pub cores: Option<u64>,
    pub device: Option<Value>,
    pub driver: Option<String>,
}

/// A restart or reschedule policy of `failures`: its attempts and the other fields as given,
/// with durations in nanoseconds, as the API takes them.
#[derive(Debug, Default)]
pub struct Policy {
    pub attempts: Option<u32>,
    pub fields: Map<String, Value>,
}

impl Policy {
    /// Writes the fields into a policy of the job.
    pub fn fill(&self, policy: &mut Value) {
        for (key, value) in &self.fields {
            policy[key] = value.clone();
        }
    }
}

/// Nomad's priority names in nf-nomad, with Nomad's default as `normal`.
const PRIORITIES: [(&str, u32); 5] = [
    ("critical", 100),
    ("high", 80),
    ("normal", 50),
    ("low", 30),
    ("min", 1),
];

impl Options {
    /// The options of the task; none when it has no `nomadOptions`.
    pub fn of(placement: &Placement) -> Result<Options, String> {
        let Some(given) = placement.options.get("nomadOptions") else {
            return Ok(Options::default());
        };
        let fail = |message: String| format!("the nomadOptions of the task {message}");
        let map = given
            .as_object()
            .ok_or_else(|| fail("must be a map".into()))?;
        let mut options = Options::default();
        for (key, value) in map {
            let wrong = |what: &str| fail(format!("take {what} for {key}"));
            match key.as_str() {
                "datacenters" => {
                    options.datacenters = texts(value).ok_or_else(|| wrong("a list of texts"))?
                }
                "nodePool" => options.node_pool = Some(text(value).ok_or_else(|| wrong("text"))?),
                "priority" => options.priority = Some(priority(value).ok_or_else(|| wrong("critical, high, normal, low, min or 1 to 100"))?),
                "meta" => {
                    let map = value.as_object().ok_or_else(|| wrong("a map"))?;
                    for (name, value) in map {
                        options.meta.insert(name.clone(), text(value).unwrap_or_default());
                    }
                }
                "constraints" | "constraint" => {
                    options.constraints = rules(Some(value.clone()), constraint).map_err(|_| {
                        fail("give constraints as a list of maps with attribute, operator and value; uwe runs no constraint closures".into())
                    })?
                }
                "affinity" | "affinities" => {
                    options.affinities = rules(Some(value.clone()), affinity).map_err(fail)?
                }
                "spread" | "spreads" => {
                    options.spreads = rules(Some(value.clone()), spread).map_err(fail)?
                }
                "volumes" | "volume" => {
                    let list = match value {
                        Value::Object(_) => json!([value]),
                        _ => value.clone(),
                    };
                    options.volumes = volume_list(&list).map_err(fail)?
                }
                "shutdownDelay" => {
                    options.shutdown_delay = Some(nanos(value).ok_or_else(|| wrong("a duration"))?)
                }
                "failures" => {
                    let map = value.as_object().ok_or_else(|| wrong("a map"))?;
                    for (kind, policy) in map {
                        let slot = match kind.as_str() {
                            "restart" => &mut options.restart,
                            "reschedule" => &mut options.reschedule,
                            _ => return Err(fail(format!("have no failures.{kind}"))),
                        };
                        *slot = failure_policy(policy).map_err(fail)?;
                    }
                }
                "resources" => {
                    let map = value.as_object().ok_or_else(|| wrong("a map"))?;
                    for (name, value) in map {
                        match name.as_str() {
                            "memoryMax" => options.memory_max = Some(mebibytes(value).ok_or_else(|| wrong("a memory size"))?),
                            "cpu" => options.cpu = Some(value.as_u64().ok_or_else(|| wrong("MHz for resources.cpu"))?),
                            "cores" => options.cores = Some(value.as_u64().ok_or_else(|| wrong("a number for resources.cores"))?),
                            "device" => options.device = Some(device(value).ok_or_else(|| wrong("a device with name and count"))?),
                            _ => return Err(fail(format!("have no resources.{name}"))),
                        }
                    }
                    if options.cpu.is_some() && options.cores.is_some() {
                        return Err(fail("take resources.cpu or resources.cores, not both".into()));
                    }
                }
                "driver" => {
                    let name = text(value).ok_or_else(|| wrong("text"))?;
                    options.driver = Some(crate::settings::driver_name(&name).map_err(fail)?);
                }
                "privileged" if value.as_bool() == Some(true) => {
                    return Err(fail(format!("ask for privileged containers; {}", crate::PRIVILEGED)));
                }
                "privileged" => {}
                "namespace" | "secrets" | "secretsPath" => {
                    return Err(fail(format!("set {key}, which uwe takes only from the executor settings")));
                }
                _ => return Err(fail(format!("have the unknown option {key}"))),
            }
        }
        Ok(options)
    }
}

fn failure_policy(value: &Value) -> Result<Policy, String> {
    let map = value
        .as_object()
        .ok_or("take a map for each failures policy")?;
    let mut policy = Policy::default();
    for (key, value) in map {
        let field = match key.as_str() {
            "attempts" => {
                let attempts = value.as_u64().and_then(|count| u32::try_from(count).ok());
                policy.attempts = Some(attempts.ok_or("take a number of attempts")?);
                continue;
            }
            "delay" | "interval" | "maxDelay" => {
                let nanos = nanos(value).ok_or("take durations like '10s'")?;
                let field = format!("{}{}", key[..1].to_ascii_uppercase(), &key[1..]);
                policy.fields.insert(field, json!(nanos));
                continue;
            }
            "mode" => "Mode",
            "delayFunction" => "DelayFunction",
            "unlimited" => "Unlimited",
            "renderTemplates" => "RenderTemplates",
            _ => return Err(format!("have no failures option {key}")),
        };
        policy.fields.insert(field.into(), value.clone());
    }
    Ok(policy)
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn texts(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) => items.iter().map(text).collect(),
        value => text(value).map(|text| vec![text]),
    }
}

fn priority(value: &Value) -> Option<u32> {
    if let Some(number) = value.as_u64() {
        return u32::try_from(number)
            .ok()
            .filter(|number| (1..=100).contains(number));
    }
    let name = value.as_str()?;
    PRIORITIES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, value)| *value)
}

fn device(value: &Value) -> Option<Value> {
    let map = value.as_object()?;
    let name = map.get("name")?.as_str()?;
    let count = map.get("count").map_or(Some(1), Value::as_u64)?;
    Some(json!({"Name": name, "Count": count}))
}

/// A duration in nanoseconds: a number of milliseconds, as nf-nomad's model takes it, or a text
/// like `10s`, `500ms`, `2m` or `1h`.
fn nanos(value: &Value) -> Option<u64> {
    if let Some(millis) = value.as_u64() {
        return millis.checked_mul(1_000_000);
    }
    let text = value.as_str()?.trim().replace(' ', "");
    let split = text.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale = match unit {
        "ms" => 1e6,
        "s" | "sec" => 1e9,
        "m" | "min" => 60e9,
        "h" => 3600e9,
        "d" => 86_400e9,
        _ => return None,
    };
    Some((number * scale) as u64)
}

/// A memory size in MiB: a number of MiB, or a text like `4 GB` or `512 MB`.
fn mebibytes(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let text = value.as_str()?.trim().to_ascii_uppercase().replace(' ', "");
    let split = text.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale = match unit {
        "MB" | "M" => 1.0,
        "GB" | "G" => 1024.0,
        "TB" | "T" => 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * scale).ceil() as u64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_task_options() {
        let mut placement = Placement::default();
        let options = json!({
            "nodePool": "highmem", "meta": {"owner": "team-x"}, "priority": "high",
            "failures": {"reschedule": {"attempts": 2, "delay": "10s"}},
            "resources": {"memoryMax": "4 GB", "cores": 3},
        });
        placement.options.insert("nomadOptions".into(), options);
        let read = Options::of(&placement).expect("options");
        assert_eq!(read.node_pool.as_deref(), Some("highmem"));
        assert_eq!(read.priority, Some(80));
        assert_eq!(read.reschedule.attempts, Some(2));
        assert_eq!(read.reschedule.fields["Delay"], 10_000_000_000u64);
        assert_eq!((read.memory_max, read.cores), (Some(4096), Some(3)));
        for wrong in [
            json!({"constraints": null}),
            json!({"privileged": true}),
            json!({"secrets": ["TOKEN"]}),
            json!({"resources": {"cpu": 1000, "cores": 2}}),
            json!({"nodes": 1}),
        ] {
            placement
                .options
                .insert("nomadOptions".into(), wrong.clone());
            assert!(Options::of(&placement).is_err(), "{wrong}");
        }
    }
}
