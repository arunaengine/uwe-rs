//! Azure Batch pools: the body that creates one, its image and auto scale formula, the checks of
//! a pool before use and while tasks wait on it, and the slots that a task takes on its nodes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use task_model::Placement;

use crate::settings::{PoolSettings, Registry};

/// The VM size of pools without one, and its cores.
const DEFAULT_SIZE: (&str, u32) = ("Standard_D4a_v4", 4);

/// The most task slots that Azure Batch gives a node.
const MOST_SLOTS: u32 = 256;

/// Resize errors after which an auto scaling pool tries again by itself.
const RECOVERABLE: [&str; 2] = ["AllocationTimedout", "AllocationFailed"];

/// What uwe knows of a pool that it runs tasks on.
#[derive(Clone, Debug, PartialEq)]
pub struct Pool {
    pub id: String,
    pub vm_size: String,
    /// The task slots of a node, which are the cores of its VM size.
    pub slots: u32,
    pub memory_gb: Option<f64>,
    pub auto_scale: bool,
    /// The user account of the pool that tasks run as.
    pub run_as: Option<String>,
}

/// How a pool that tasks wait on is doing.
#[derive(Debug, PartialEq, Eq)]
pub enum Health {
    Fine,
    /// It has no nodes but tries again; the text is a warning.
    Waiting(String),
    /// It will get no nodes; the text is the error of the waiting tasks.
    Broken(String),
}

/// Refuses an id that Azure Batch does not take.
pub fn valid_id(id: &str) -> Result<(), String> {
    let plain = (id.bytes()).all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte));
    match plain && !id.is_empty() && id.len() <= 64 {
        true => Ok(()),
        false => Err(format!(
            "Invalid Azure Batch pool Id '{id}' - It can only contain alphanumeric, hyphen and \
             underscore characters"
        )),
    }
}

/// The id of the auto pool of `settings` and `labels`: every other VM size, setting or label
/// set gets its own pool, as in Nextflow.
pub fn auto_id(settings: &PoolSettings, labels: &BTreeMap<String, String>) -> String {
    let digest = Sha256::digest(format!("{settings:?}{labels:?}").as_bytes());
    let hex: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let size: String = (settings.vm_size.chars())
        .map(
            |letter| match letter.is_ascii_alphanumeric() || letter == '-' {
                true => letter,
                false => '_',
            },
        )
        .collect();
    let mut id = format!("uwe-pool-{hex}-{size}");
    id.truncate(64);
    id
}

/// The cores of the VM size of `settings`, which uwe must know to create a pool.
pub fn cores(settings: &PoolSettings, id: &str) -> Result<u32, String> {
    match settings.vm_cpus {
        Some(cpus) => Ok(cpus),
        None if settings.vm_size.eq_ignore_ascii_case(DEFAULT_SIZE.0) => Ok(DEFAULT_SIZE.1),
        None => Err(format!(
            "uwe needs the cores of the VM size {} to create the Azure Batch pool '{id}': set \
             vm_cpus in [executor.azure-batch.pools.{id}]",
            settings.vm_size
        )),
    }
}

/// The image of a new pool: the custom image of `settings`, or the first verified Linux image
/// of the account that matches its publisher, offer and node agent.
pub fn image(images: &[Value], settings: &PoolSettings) -> Result<(Value, String), String> {
    if let Some(id) = &settings.image_id {
        return Ok((json!({"virtualMachineImageId": id}), settings.sku.clone()));
    }
    let same = |value: &Value, wanted: &str| {
        value
            .as_str()
            .is_some_and(|text| text.eq_ignore_ascii_case(wanted))
    };
    let found = images.iter().find(|image| {
        let reference = &image["imageReference"];
        same(&image["nodeAgentSKUId"], &settings.sku)
            && same(&image["osType"], "linux")
            && (settings.unverified_images || same(&image["verificationType"], "verified"))
            && same(&reference["publisher"], &settings.publisher)
            && same(&reference["offer"], &settings.offer)
    });
    let Some(found) = found else {
        return Err(format!(
            "Cannot find a matching VM image with publisher={}; offer={}; OS type=linux; allow \
             unverified images={}",
            settings.publisher, settings.offer, settings.unverified_images
        ));
    };
    let agent = found["nodeAgentSKUId"].as_str().unwrap_or(&settings.sku);
    Ok((found["imageReference"].clone(), agent.to_owned()))
}

/// The parts of a new pool besides its settings.
pub struct Creation<'a> {
    pub id: &'a str,
    pub cores: u32,
    pub image: (Value, String),
    pub registry: Option<(&'a Registry, String)>,
    pub labels: &'a BTreeMap<String, String>,
    /// The time of the creation, for the auto scale formula, as an ISO 8601 instant.
    pub now: &'a str,
}

/// The body that creates a pool.
pub fn body(creation: &Creation<'_>, settings: &PoolSettings) -> Value {
    let (reference, agent) = &creation.image;
    let mut container = json!({"type": "dockerCompatible"});
    if let Some((registry, password)) = &creation.registry {
        container["containerRegistries"] = json!([{
            "registryServer": registry.server,
            "username": registry.user,
            "password": password,
        }]);
    }
    let mut body = json!({
        "id": creation.id,
        "vmSize": settings.vm_size,
        "virtualMachineConfiguration": {
            "imageReference": reference,
            "nodeAgentSKUId": agent,
            "containerConfiguration": container,
        },
        "taskSlotsPerNode": creation.cores.clamp(1, MOST_SLOTS),
    });
    if let Some(script) = &settings.start_script {
        body["startTask"] = json!({
            "commandLine": "/bin/sh -c 'eval \"$UWE_START\"'",
            "environmentSettings": [{"name": "UWE_START", "value": script}],
            "userIdentity": {"autoUser": {"scope": "pool", "elevationLevel": "nonadmin"}},
            "waitForSuccess": true,
        });
    }
    if !creation.labels.is_empty() {
        let pairs = creation.labels.iter();
        body["metadata"] = pairs
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect();
    }
    if let Some(subnet) = &settings.virtual_network {
        body["networkConfiguration"] = json!({"subnetId": subnet});
    }
    if let Some(policy) = &settings.schedule_policy {
        body["taskSchedulingPolicy"] = json!({"nodeFillType": policy});
    }
    if settings.auto_scale {
        let minutes = settings.scale_interval.as_secs() / 60;
        body["enableAutoScale"] = json!(true);
        body["autoScaleEvaluationInterval"] = json!(format!("PT{minutes}M"));
        body["autoScaleFormula"] = json!(formula(settings, creation.now));
    } else if settings.low_priority {
        body["targetLowPriorityNodes"] = json!(settings.vm_count);
    } else {
        body["targetDedicatedNodes"] = json!(settings.vm_count);
    }
    body
}

/// The auto scale formula of a pool: `vm_count` nodes for the first interval, then as many as
/// tasks wait, or half the target when none wait, at most `max_vm_count`.
pub fn formula(settings: &PoolSettings, created: &str) -> String {
    let own = "$interval = TimeInterval_Minute * {{scaleInterval}};\n\
               $young = time() - time(\"{{poolCreationTime}}\") < $interval;\n\
               $known = $PendingTasks.GetSamplePercent($interval) >= 70;\n\
               $waiting = $known ? max($PendingTasks.GetSample(1), avg($PendingTasks.GetSample($interval))) : max(0, $PendingTasks.GetSample(1));\n\
               $wanted = $waiting > 0 ? $waiting : max(0, $TARGET / 2);\n\
               $TARGET = $young ? {{vmCount}} : min({{maxVmCount}}, $wanted);\n\
               $NodeDeallocationOption = taskcompletion;";
    let target = match settings.low_priority {
        true => "TargetLowPriorityNodes",
        false => "TargetDedicatedNodes",
    };
    let most = (settings.max_vm_count).unwrap_or(settings.vm_count.saturating_mul(3));
    (settings.scale_formula.as_deref().unwrap_or(own))
        .replace("$TARGET", &format!("${target}"))
        .replace(
            "{{scaleInterval}}",
            &(settings.scale_interval.as_secs() / 60).to_string(),
        )
        .replace("{{poolCreationTime}}", created)
        .replace("{{vmCount}}", &settings.vm_count.to_string())
        .replace("{{maxVmCount}}", &most.to_string())
}

/// Checks a pool that exists before its first task, against what the settings know of it.
pub fn check(found: &Value, id: &str, settings: &PoolSettings) -> Result<Pool, String> {
    if found["state"].as_str() != Some("active") {
        return Err(format!("Azure Batch pool '{id}' not in active state"));
    }
    let slots = (found["taskSlotsPerNode"].as_u64())
        .and_then(|slots| u32::try_from(slots).ok())
        .unwrap_or(1);
    if let Some(cores) = settings.vm_cpus
        && cores != slots
    {
        return Err(format!(
            "Azure Batch pool '{id}' slots per node does not match the VM num cores (slots: \
             {slots}, cores: {cores})"
        ));
    }
    Ok(Pool {
        id: id.to_owned(),
        vm_size: found["vmSize"].as_str().unwrap_or_default().to_owned(),
        slots,
        memory_gb: settings.vm_memory_gb,
        auto_scale: found["enableAutoScale"].as_bool().unwrap_or(false),
        run_as: settings.run_as.clone(),
    })
}

/// Whether the pool that a task waits on can still get nodes.
pub fn health(found: &Value, pool: &Pool) -> Health {
    let errors = found["resizeErrors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let nodes = ["currentDedicatedNodes", "currentLowPriorityNodes"]
        .iter()
        .map(|key| found[key].as_u64().unwrap_or(0))
        .sum::<u64>();
    let steady = found["allocationState"].as_str() == Some("steady");
    if errors.is_empty() || nodes > 0 || !steady {
        return Health::Fine;
    }
    let codes: Vec<&str> = errors
        .iter()
        .filter_map(|error| error["code"].as_str())
        .collect();
    if pool.auto_scale && codes.iter().all(|code| RECOVERABLE.contains(code)) {
        return Health::Waiting(format!(
            "Azure Batch pool '{}' has recoverable resize errors and no agents; waiting for a \
             fresh resize - [{}]",
            pool.id,
            codes.join(", ")
        ));
    }
    let described: Vec<String> = (errors.iter())
        .map(|error| {
            let code = error["code"].as_str().unwrap_or_default();
            format!("{code}: {}", error["message"].as_str().unwrap_or_default())
        })
        .collect();
    Health::Broken(format!(
        "Azure Batch pool '{}' has resize errors and no agents are available - {}",
        pool.id,
        described.join("; ")
    ))
}

/// The task slots of a task: the most of its cpus and its share of the memory of a node, at
/// most the slots of a node.
pub fn slots(placement: &Placement, pool: &Pool) -> u32 {
    let mut slots = placement.cpus.max(1);
    if let (Some(bytes), Some(memory)) = (placement.memory_bytes, pool.memory_gb) {
        let per_slot = memory / f64::from(pool.slots.max(1));
        let wanted = (bytes as f64 / f64::from(1 << 30)).min(memory);
        slots = slots.max((wanted / per_slot).ceil() as u32);
    }
    slots.clamp(1, pool.slots.clamp(1, MOST_SLOTS))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn pool(slots: u32, auto_scale: bool) -> Pool {
        Pool {
            id: "p".into(),
            vm_size: "Standard_E4d_v5".into(),
            slots,
            memory_gb: Some(32.0),
            auto_scale,
            run_as: None,
        }
    }

    #[test]
    fn builds_pool_bodies() {
        let settings = PoolSettings {
            auto_scale: true,
            vm_count: 2,
            low_priority: true,
            schedule_policy: Some("pack".into()),
            start_script: Some("echo ready".into()),
            scale_interval: Duration::from_secs(600),
            ..PoolSettings::default()
        };
        let images = [
            json!({"nodeAgentSKUId": "batch.node.ubuntu 24.04", "osType": "linux", "verificationType": "unverified", "imageReference": {"publisher": "microsoft-dsvm", "offer": "ubuntu-hpc", "sku": "old"}}),
            json!({"nodeAgentSKUId": "Batch.Node.Ubuntu 24.04", "osType": "linux", "verificationType": "verified", "imageReference": {"publisher": "Microsoft-DSVM", "offer": "ubuntu-hpc", "sku": "2404"}}),
        ];
        let image = image(&images, &settings).expect("an image");
        assert_eq!(image.0["sku"], "2404");
        let labels = BTreeMap::from([("team".to_owned(), "genomics".to_owned())]);
        let id = auto_id(&settings, &labels);
        assert!(
            id.starts_with("uwe-pool-") && id.ends_with("-Standard_D4a_v4"),
            "{id}"
        );
        assert_ne!(
            id,
            auto_id(&settings, &BTreeMap::new()),
            "labels make another pool"
        );
        let creation = Creation {
            id: &id,
            cores: cores(&settings, &id).expect("cores"),
            image,
            registry: None,
            labels: &labels,
            now: "2026-11-06T10:00:00.000000Z",
        };
        let body = body(&creation, &settings);
        assert_eq!(body["taskSlotsPerNode"], 4);
        assert_eq!(body["autoScaleEvaluationInterval"], "PT10M");
        assert_eq!(
            body["metadata"],
            json!([{"name": "team", "value": "genomics"}])
        );
        assert_eq!(body["taskSchedulingPolicy"]["nodeFillType"], "pack");
        let start = &body["startTask"]["userIdentity"]["autoUser"];
        assert_eq!(start["elevationLevel"], "nonadmin");
        let formula = body["autoScaleFormula"].as_str().expect("a formula");
        assert!(
            formula.contains("$TargetLowPriorityNodes = $young ? 2 : min(6, $wanted);"),
            "{formula}"
        );
        assert!(
            formula.contains("time(\"2026-11-06T10:00:00.000000Z\")"),
            "{formula}"
        );
        let unverified = PoolSettings {
            offer: "other".into(),
            ..settings
        };
        let error = super::image(&images, &unverified).expect_err("no image");
        assert!(
            error.starts_with("Cannot find a matching VM image with publisher="),
            "{error}"
        );
        let sized = PoolSettings {
            vm_size: "Standard_E2d_v5".into(),
            ..PoolSettings::default()
        };
        assert!(cores(&sized, "p").is_err());
        assert!(valid_id("pool_1-a").is_ok() && valid_id("pool 1").is_err());
    }

    #[test]
    fn checks_existing_pools() {
        let settings = PoolSettings {
            vm_cpus: Some(4),
            ..PoolSettings::default()
        };
        let found = json!({"state": "active", "taskSlotsPerNode": 4, "vmSize": "standard_e4d_v5"});
        assert_eq!(check(&found, "p", &settings).map(|pool| pool.slots), Ok(4));
        let deleting = json!({"state": "deleting", "taskSlotsPerNode": 4});
        assert!(
            check(&deleting, "p", &settings).is_err_and(|error| error.contains("not in active"))
        );
        let wide = json!({"state": "active", "taskSlotsPerNode": 8});
        assert!(
            check(&wide, "p", &settings).is_err_and(|error| error.contains("slots: 8, cores: 4"))
        );
        let failed = json!({"allocationState": "steady", "currentDedicatedNodes": 0, "resizeErrors": [{"code": "AllocationFailed", "message": "no VMs"}]});
        assert!(matches!(
            health(&failed, &pool(4, true)),
            Health::Waiting(_)
        ));
        let Health::Broken(error) = health(&failed, &pool(4, false)) else {
            panic!("a broken pool");
        };
        assert!(
            error.ends_with("no agents are available - AllocationFailed: no VMs"),
            "{error}"
        );
        assert_eq!(
            health(&json!({"allocationState": "steady"}), &pool(4, false)),
            Health::Fine
        );
    }

    #[test]
    fn counts_task_slots() {
        let placement = Placement {
            cpus: 1,
            memory_bytes: Some(20 << 30),
            ..Placement::default()
        };
        assert_eq!(
            slots(&placement, &pool(4, false)),
            3,
            "20 GB of 8 GB per slot"
        );
        let wide = Placement {
            cpus: 16,
            ..Placement::default()
        };
        assert_eq!(slots(&wide, &pool(4, false)), 4);
    }
}
