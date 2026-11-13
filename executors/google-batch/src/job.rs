//! The job of one attempt: its id, the runnables that get the helper and run the task, its
//! resources and time, spot retries, the VMs, network, labels and logs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};
use task_model::Placement;

use crate::settings::Settings;

/// Gets the helper `$2` from `$3` and checks its SHA-256 `$4`, in the folder `$1` that the task
/// container sees too and may write to.
const DOWNLOAD: &str = "mkdir -p \"$1\" && chmod 1777 \"$1\" && wget -q -O \"$2\" \"$3\" && \
                        echo \"$4  $2\" | sha256sum -c - > /dev/null && chmod 755 \"$2\" || \
                        { echo \"uwe: cannot get the helper\" >&2; exit 125; }\n";

/// One attempt as Google Batch runs it.
#[derive(Debug)]
pub struct Shape<'a> {
    /// The display name of the task, for warnings.
    pub task: &'a str,
    pub image: &'a str,
    /// The commands of the task container.
    pub commands: Vec<String>,
    /// Host folders that the task container sees, as `docker run --volume` takes them.
    pub volumes: Vec<String>,
    /// The download of the helper before the task, when the VM does not have it.
    pub download: Option<Download<'a>>,
    pub placement: &'a Placement,
    /// The time of the task with the staging margin, in seconds.
    pub seconds: Option<u64>,
}

/// A first runnable that downloads the helper to `path`.
#[derive(Debug)]
pub struct Download<'a> {
    pub image: &'a str,
    pub path: &'a str,
    pub url: &'a str,
    pub sha256: &'a str,
    /// The machine of the helper, as `uname -m` names it, when that is known.
    pub machine: Option<&'a str>,
}

/// The id of the job of the attempt `name`: `uwe-`, the name in lower case with each run of
/// other characters than letters and digits as one `-`, and the Unix time in milliseconds, at
/// most 63 characters, as Google takes them.
pub fn job_id(name: &str, millis: u128) -> String {
    let mut clean = String::new();
    for character in name.chars().map(|character| character.to_ascii_lowercase()) {
        match character {
            'a'..='z' | '0'..='9' => clean.push(character),
            _ if !clean.ends_with('-') => clean.push('-'),
            _ => {}
        }
    }
    let millis = millis.to_string();
    let room = 63 - "uwe-".len() - millis.len() - 1;
    let clean: String = clean.trim_matches('-').chars().take(room).collect();
    format!("uwe-{}-{millis}", clean.trim_end_matches('-')).replace("--", "-")
}

/// The body of the job.
pub fn body(shape: &Shape<'_>, settings: &Settings) -> Result<Value, String> {
    let placement = shape.placement;
    let mut runnables = Vec::new();
    if let Some(download) = &shape.download {
        runnables.push(fetch(download, &settings.task_folder));
    }
    runnables.push(json!({
        "container": {
            "imageUri": shape.image,
            "commands": shape.commands,
            "volumes": shape.volumes,
        },
    }));
    let mut resources = json!({"cpuMilli": u64::from(placement.cpus.max(1)) * 1000});
    if let Some(bytes) = placement.memory_bytes {
        resources["memoryMib"] = json!(bytes.div_ceil(1 << 20));
    }
    let boot = (placement.disk_bytes.map(|bytes| bytes.div_ceil(1 << 20)))
        .or(settings.boot_disk_gb.map(|size| size * 1024));
    if let Some(boot) = boot {
        resources["bootDiskMib"] = json!(boot);
    }
    let mut spec = json!({"runnables": runnables, "computeResource": resources});
    if let Some(seconds) = shape.seconds {
        spec["maxRunDuration"] = json!(format!("{seconds}s"));
    }
    if settings.spot_attempts > 0 {
        spec["maxRetryCount"] = json!(settings.spot_attempts);
        spec["lifecyclePolicies"] = json!([{
            "action": "RETRY_TASK",
            "actionCondition": {"exitCodes": settings.retry_codes},
        }]);
    }
    let mut logs = json!({"destination": "CLOUD_LOGGING"});
    if let Some(path) = &settings.logs_path {
        // Google Batch writes the logs to a path of the VM, where the bucket is mounted.
        let path = path.trim_start_matches("gs://").trim_end_matches('/');
        let bucket = path.split('/').next().unwrap_or_default();
        let mount = format!("/mnt/disks/{bucket}");
        spec["volumes"] = json!([{"gcs": {"remotePath": bucket}, "mountPath": mount}]);
        logs = json!({"destination": "PATH", "logsPath": format!("/mnt/disks/{path}")});
    }
    let labels = json!(placement.labels);
    let mut allocation = json!({"instances": [instance(shape, settings)?]});
    if let Some(email) = &settings.service_account {
        allocation["serviceAccount"] = json!({"email": email});
    }
    if !placement.labels.is_empty() {
        allocation["labels"] = labels.clone();
    }
    if !settings.network_tags.is_empty() && !template(settings) {
        allocation["tags"] = json!(settings.network_tags);
    }
    if !settings.allowed_locations.is_empty() {
        allocation["location"] = json!({"allowedLocations": settings.allowed_locations});
    }
    if settings.network.is_some() || settings.subnetwork.is_some() || settings.private_address {
        let mut interface = json!({});
        if let Some(network) = &settings.network {
            interface["network"] = json!(network);
        }
        if let Some(subnetwork) = &settings.subnetwork {
            interface["subnetwork"] = json!(subnetwork);
        }
        if settings.private_address {
            interface["noExternalIpAddress"] = json!(true);
        }
        allocation["network"] = json!({"networkInterfaces": [interface]});
    }
    let mut body = json!({
        "taskGroups": [{"taskCount": 1, "taskSpec": spec}],
        "allocationPolicy": allocation,
        "logsPolicy": logs,
    });
    if !placement.labels.is_empty() {
        body["labels"] = labels;
    }
    Ok(body)
}

/// The runnable that downloads the helper, as the user of its image.
fn fetch(download: &Download<'_>, folder: &str) -> Value {
    let mut script = String::new();
    // A downloaded binary runs only on its own machine.
    if let Some(machine) = download.machine {
        script.push_str(&format!(
            "[ \"$(uname -m)\" = {machine} ] || {{ echo \"uwe: the helper runs on {machine}, \
             but this VM is $(uname -m)\" >&2; exit 125; }}\n"
        ));
    }
    script.push_str(DOWNLOAD);
    let words = [folder, download.path, download.url, download.sha256];
    let mut commands = vec!["-c".to_owned(), script, "sh".to_owned()];
    commands.extend(words.map(str::to_owned));
    json!({
        "container": {
            "imageUri": download.image,
            "entrypoint": "sh",
            "commands": commands,
            "volumes": [format!("{folder}:{folder}")],
        },
    })
}

/// The VMs of the job: an instance template, or a policy of machine type, spot, CPU platform,
/// GPUs and boot disk.
fn instance(shape: &Shape<'_>, settings: &Settings) -> Result<Value, String> {
    let gpus: u32 = shape
        .placement
        .accelerators
        .iter()
        .map(|gpu| gpu.count)
        .sum();
    let task = shape.task;
    if let Some(name) =
        (settings.machine_type.as_deref()).and_then(|machine| machine.strip_prefix("template://"))
    {
        let ignored = [
            ("the GPUs of the task", gpus > 0),
            ("boot_disk_image", settings.boot_disk_image.is_some()),
            ("cpu_platform", settings.cpu_platform.is_some()),
            ("network_tags", !settings.network_tags.is_empty()),
            (
                "spot",
                settings.spot || settings.preemptible || shape.placement.preemptible,
            ),
        ];
        for (what, _) in ignored.iter().filter(|(_, set)| *set) {
            tracing::warn!(
                task,
                "uwe ignores {what}, as an instance template sets the VMs"
            );
        }
        let mut instance = json!({"instanceTemplate": name});
        if settings.gpu_drivers {
            instance["installGpuDrivers"] = json!(true);
        }
        return Ok(instance);
    }
    let mut policy = json!({});
    if let Some(machine) = &settings.machine_type {
        policy["machineType"] = json!(machine);
    }
    if settings.spot || shape.placement.preemptible {
        policy["provisioningModel"] = json!("SPOT");
    } else if settings.preemptible {
        policy["provisioningModel"] = json!("PREEMPTIBLE");
    }
    if let Some(platform) = &settings.cpu_platform {
        policy["minCpuPlatform"] = json!(platform);
    }
    if let Some(image) = &settings.boot_disk_image {
        policy["bootDisk"] = json!({"image": image});
    }
    let mut instance = json!({});
    if gpus > 0 {
        let kind = (shape.placement.accelerators.iter())
            .find_map(|gpu| gpu.kind.as_deref())
            .or(settings.accelerator_type.as_deref())
            .ok_or_else(|| {
                format!(
                    "task {task} asks for GPUs, but Google Batch needs their type: set \
                     accelerator_type in [executor.google-batch]"
                )
            })?;
        policy["accelerators"] = json!([{"type": kind, "count": gpus}]);
        instance["installGpuDrivers"] = json!(true);
    }
    if settings.ops_agent {
        instance["installOpsAgent"] = json!(true);
        if (settings.boot_disk_image.as_deref()).is_some_and(|image| !image.contains("debian")) {
            tracing::warn!(
                "The Ops Agent requires a compatible boot disk image; set boot_disk_image to a \
                 batch-debian image"
            );
        }
    }
    instance["policy"] = policy;
    Ok(instance)
}

fn template(settings: &Settings) -> bool {
    (settings.machine_type.as_deref()).is_some_and(|machine| machine.starts_with("template://"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_model::Accelerator;

    #[test]
    fn names_jobs() {
        assert_eq!(job_id("run-ALIGN (1)-1-1", 7), "uwe-run-align-1-1-1-7");
        let long = job_id(&"Task_".repeat(30), 1_762_400_000_000);
        assert!(long.len() <= 63, "{long}");
        assert!(long.starts_with("uwe-task-task-") && long.ends_with("-1762400000000"));
        assert_eq!(job_id("..", 1), "uwe-1");
    }

    #[test]
    fn builds_job_bodies() {
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3 << 30),
            disk_bytes: Some(50 << 30),
            accelerators: vec![Accelerator {
                count: 1,
                kind: None,
            }],
            labels: [("team".to_owned(), "genomics".to_owned())].into(),
            ..Placement::default()
        };
        let download = Download {
            image: "alpine",
            path: "/tmp/uwe/a.uwe",
            url: "https://staging/uwe",
            sha256: "ab",
            machine: Some("x86_64"),
        };
        let shape = Shape {
            task: "ALIGN",
            image: "ubuntu",
            commands: vec!["sh".into()],
            volumes: vec!["/tmp/uwe:/tmp/uwe".into()],
            download: Some(download),
            placement: &placement,
            seconds: Some(3600),
        };
        let mut settings = Settings {
            spot: true,
            spot_attempts: 3,
            service_account: Some("jobs@lab.iam.gserviceaccount.com".into()),
            private_address: true,
            ..Settings::default()
        };
        let error = body(&shape, &settings).expect_err("a GPU without type");
        assert!(error.contains("accelerator_type"), "{error}");
        settings.accelerator_type = Some("nvidia-tesla-t4".into());
        let body = body(&shape, &settings).expect("a body");
        let spec = &body["taskGroups"][0]["taskSpec"];
        let resources = json!({"cpuMilli": 2000, "memoryMib": 3072, "bootDiskMib": 51200});
        assert_eq!(spec["computeResource"], resources);
        assert_eq!(spec["maxRunDuration"], "3600s");
        assert_eq!(spec["maxRetryCount"], 3);
        assert_eq!(
            spec["lifecyclePolicies"][0]["actionCondition"]["exitCodes"],
            json!([50001])
        );
        let fetch = &spec["runnables"][0]["container"];
        assert_eq!(fetch["imageUri"], "alpine");
        let commands = fetch["commands"].as_array().expect("commands");
        assert_eq!(
            commands[3..],
            [
                json!("/tmp/uwe"),
                json!("/tmp/uwe/a.uwe"),
                json!("https://staging/uwe"),
                json!("ab")
            ]
        );
        assert!(
            commands[1]
                .as_str()
                .is_some_and(|script| script.contains("= x86_64 ]"))
        );
        assert_eq!(spec["runnables"][1]["container"]["imageUri"], "ubuntu");
        let allocation = &body["allocationPolicy"];
        let instance = &allocation["instances"][0];
        assert_eq!(instance["policy"]["provisioningModel"], "SPOT");
        let gpu = json!([{"type": "nvidia-tesla-t4", "count": 1}]);
        assert_eq!(instance["policy"]["accelerators"], gpu);
        assert_eq!(instance["installGpuDrivers"], true);
        assert_eq!(allocation["labels"]["team"], "genomics");
        assert_eq!(body["labels"]["team"], "genomics");
        let interface = &allocation["network"]["networkInterfaces"][0];
        assert_eq!(interface["noExternalIpAddress"], true);
        assert_eq!(body["logsPolicy"]["destination"], "CLOUD_LOGGING");
        let templated = Settings {
            machine_type: Some("template://gpu-vm".into()),
            logs_path: Some("gs://logs/uwe".into()),
            ..settings
        };
        let body = super::body(&shape, &templated).expect("a body");
        let instance = &body["allocationPolicy"]["instances"][0];
        assert_eq!(instance, &json!({"instanceTemplate": "gpu-vm"}));
        let logs = json!({"destination": "PATH", "logsPath": "/mnt/disks/logs/uwe"});
        assert_eq!(body["logsPolicy"], logs);
    }
}
