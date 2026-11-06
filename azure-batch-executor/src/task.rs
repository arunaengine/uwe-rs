//! The Azure Batch task of a task attempt: its id, the container and its options, the helper and
//! plan as resource files, the command line, slots, time and user; and the ids of jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use task_model::Placement;

/// The environment variable that holds the job script, as a command line holds no newlines.
pub const SCRIPT: &str = "UWE_SCRIPT";

/// The longest id that Azure Batch takes.
const LONGEST: usize = 64;

/// A file that Azure Batch downloads into the working folder of the task before it starts.
#[derive(Debug)]
pub struct Download {
    /// A URL that grants reading this one file, like a blob URL with a SAS.
    pub url: String,
    pub path: &'static str,
    pub executable: bool,
}

/// What a task holds besides its settings.
#[derive(Debug)]
pub struct Shape<'a> {
    pub id: &'a str,
    pub image: &'a str,
    /// The job command, as [`executor::batch::command`] gives it.
    pub command: Vec<String>,
    pub downloads: Vec<Download>,
    /// Host folders that the container sees, as `host:container:ro`.
    pub volumes: Vec<String>,
    pub placement: &'a Placement,
    pub slots: u32,
    /// How long the task may run, with its staging.
    pub seconds: Option<u64>,
    /// The user account of the pool that the task runs as.
    pub run_as: Option<&'a str>,
}

/// The body that adds a task. The task runs as a user without rights, never privileged.
pub fn body(shape: &Shape<'_>) -> Result<Value, String> {
    let [shell, flag, script, rest @ ..] = shape.command.as_slice() else {
        return Err("the job command is too short".into());
    };
    let mut line = format!("{shell} {flag} 'eval \"${SCRIPT}\"'");
    for word in rest {
        if word.contains('\'') {
            return Err(format!(
                "Azure Batch cannot pass the word {word:?} to the task"
            ));
        }
        line.push_str(&format!(" '{word}'"));
    }
    let mut options = format!("--cpu-shares {}", shape.placement.cpus.max(1) * 1024);
    if let Some(bytes) = shape.placement.memory_bytes {
        options.push_str(&format!(" --memory {}m", bytes.div_ceil(1 << 20)));
    }
    for volume in &shape.volumes {
        options.push_str(&format!(" -v {volume}"));
    }
    let downloads: Vec<Value> = (shape.downloads.iter())
        .map(|download| {
            let mut file = json!({"httpUrl": download.url, "filePath": download.path});
            if download.executable {
                file["fileMode"] = json!("0755");
            }
            file
        })
        .collect();
    let user = match shape.run_as {
        Some(name) => json!({"username": name}),
        None => json!({"autoUser": {"scope": "task", "elevationLevel": "nonadmin"}}),
    };
    let mut constraints = json!({"maxTaskRetryCount": 0});
    if let Some(seconds) = shape.seconds {
        constraints["maxWallClockTime"] = json!(format!("PT{seconds}S"));
    }
    Ok(json!({
        "id": shape.id,
        "commandLine": line,
        "containerSettings": {
            "imageName": shape.image,
            "containerRunOptions": options,
            "workingDirectory": "taskWorkingDirectory",
        },
        "resourceFiles": downloads,
        "environmentSettings": [{"name": SCRIPT, "value": script}],
        "requiredSlots": shape.slots,
        "constraints": constraints,
        "userIdentity": user,
    }))
}

/// The id of the task of an attempt: its name with only the letters that Azure Batch takes,
/// and a digest of the name when it is too long.
pub fn task_id(name: &str) -> String {
    let id = plain(&format!("uwe-{name}"));
    if id.len() <= LONGEST {
        return id;
    }
    let digest = Sha256::digest(name.as_bytes());
    let hex: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}-{hex}", &id[..LONGEST - 13])
}

/// The id of the job of a run on a pool, with the random `session` of the run.
pub fn job_id(session: &str, pool: &str) -> String {
    let mut id = plain(&format!("uwe-{session}-{pool}"));
    id.truncate(LONGEST);
    id
}

fn plain(text: &str) -> String {
    (text.chars())
        .map(
            |letter| match letter.is_ascii_alphanumeric() || letter == '-' || letter == '_' {
                true => letter,
                false => '_',
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_task_bodies() {
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3 << 30),
            ..Placement::default()
        };
        let command = vec![
            "sh".into(),
            "-c".into(),
            "line one\nline two".into(),
            "uwe".into(),
            "/tmp/uwe/run-A-1-1".into(),
            "uwe-plan.json".into(),
        ];
        let mut shape = Shape {
            id: "uwe-run-A-1-1",
            image: "ubuntu:24.04",
            command,
            downloads: vec![Download {
                url: "https://lab.blob.core.windows.net/s/uwe?sig=x".into(),
                path: "uwe",
                executable: true,
            }],
            volumes: vec!["/opt:/opt:ro".into()],
            placement: &placement,
            slots: 2,
            seconds: Some(4200),
            run_as: None,
        };
        let task = body(&shape).expect("a body");
        assert_eq!(
            task["commandLine"],
            "sh -c 'eval \"$UWE_SCRIPT\"' 'uwe' '/tmp/uwe/run-A-1-1' 'uwe-plan.json'"
        );
        assert_eq!(
            task["environmentSettings"][0]["value"],
            "line one\nline two"
        );
        let container = &task["containerSettings"];
        assert_eq!(
            container["containerRunOptions"],
            "--cpu-shares 2048 --memory 3072m -v /opt:/opt:ro"
        );
        assert!(
            !container["containerRunOptions"]
                .as_str()
                .unwrap_or_default()
                .contains("privileged")
        );
        assert_eq!(task["resourceFiles"][0]["fileMode"], "0755");
        assert_eq!(task["constraints"]["maxWallClockTime"], "PT4200S");
        assert_eq!(
            task["userIdentity"]["autoUser"]["elevationLevel"],
            "nonadmin"
        );
        shape.command.push("it's".into());
        assert!(body(&shape).is_err());
        assert_eq!(task_id("run-A.b-1-1"), "uwe-run-A_b-1-1");
        let long = task_id(&"x".repeat(80));
        assert_eq!(long.len(), 64);
        assert_ne!(long, task_id(&"x".repeat(81)));
        assert_eq!(job_id("a1b2", "pool_1"), "uwe-a1b2-pool_1");
    }
}
