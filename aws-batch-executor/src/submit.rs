//! The body of `submitjob` for one attempt: its name, queue, definition, command, resources,
//! tags, spot retries, time limit and fair share; and Fargate's sizes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};
use task_model::Placement;

use crate::settings::Settings;

/// The shortest time limit of a job that AWS Batch takes, in seconds.
const LEAST_TIMEOUT: u64 = 60;

/// Below this memory in MiB, a task gets the memory of the definition, as AWS takes no less.
const LEAST_MEMORY: u64 = 4;

/// One attempt as AWS Batch runs it.
#[derive(Debug)]
pub struct Submission<'a> {
    /// The name of the attempt, which becomes the name of the job.
    pub name: &'a str,
    /// The display name of the task, for warnings.
    pub task: &'a str,
    pub queue: &'a str,
    /// `<name>:<revision>` of the job definition.
    pub definition: &'a str,
    pub command: Vec<String>,
    pub placement: &'a Placement,
    /// The time of the task with the staging margin, in seconds.
    pub seconds: Option<u64>,
}

/// The name of a job: spaces as `_`, without other characters than letters, digits, `_` and
/// `-`, at most 128 characters.
pub fn job_name(name: &str) -> String {
    (name.chars())
        .map(|character| if character == ' ' { '_' } else { character })
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(128)
        .collect()
}

/// The body of `submitjob`.
pub fn body(submission: &Submission<'_>, settings: &Settings) -> Result<Value, String> {
    let placement = submission.placement;
    let mut overrides = json!({"command": submission.command});
    let memory = placement.memory_bytes.map(|bytes| bytes >> 20);
    let mut cpus = placement.cpus.max(1);
    let mut memory = match memory {
        Some(memory) if memory < LEAST_MEMORY => {
            tracing::warn!(
                task = submission.task,
                "a memory of less than 4 MiB is left to the job definition"
            );
            None
        }
        memory => memory,
    };
    if settings.fargate {
        let (fit_cpus, fit_memory) = fargate(cpus, memory, submission.task)?;
        (cpus, memory) = (fit_cpus, Some(fit_memory));
    }
    let mut resources = Vec::new();
    if let Some(memory) = memory {
        resources.push(json!({"type": "MEMORY", "value": memory.to_string()}));
    }
    if cpus > 1 || settings.fargate {
        resources.push(json!({"type": "VCPU", "value": cpus.to_string()}));
    }
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    for kind in placement
        .accelerators
        .iter()
        .filter_map(|gpu| gpu.kind.as_ref())
    {
        tracing::warn!(
            task = submission.task,
            kind,
            "AWS Batch takes no accelerator type; the job gets any GPU of its queue"
        );
    }
    if gpus > 0 {
        if settings.fargate {
            return Err("Fargate has no GPUs; run GPU tasks on an EC2 queue".into());
        }
        resources.push(json!({"type": "GPU", "value": gpus.to_string()}));
    }
    if !resources.is_empty() {
        overrides["resourceRequirements"] = json!(resources);
    }
    let mut body = json!({
        "jobName": job_name(submission.name),
        "jobQueue": submission.queue,
        "jobDefinition": submission.definition,
        "containerOverrides": overrides,
    });
    if !placement.labels.is_empty() {
        body["tags"] = json!(placement.labels);
        body["propagateTags"] = json!(true);
    }
    if let Some(share) = &settings.share {
        body["shareIdentifier"] = json!(share);
        body["schedulingPriorityOverride"] = json!(settings.priority);
    }
    if settings.spot_attempts > 0 {
        // AWS Batch runs the job again only when its spot instance went away.
        body["retryStrategy"] = json!({
            "attempts": settings.spot_attempts,
            "evaluateOnExit": [
                {"action": "RETRY", "onStatusReason": "Host EC2*"},
                {"action": "EXIT", "onReason": "*"},
            ],
        });
    }
    if let Some(seconds) = submission.seconds {
        body["timeout"] = json!({"attemptDurationSeconds": seconds.max(LEAST_TIMEOUT)});
    }
    Ok(body)
}

/// The vCPUs and memory in MiB of a Fargate task: the vCPUs rounded up to 1, 2, 4, 8 or 16,
/// and the memory moved into the range of that size, in its steps.
pub fn fargate(cpus: u32, memory: Option<u64>, task: &str) -> Result<(u32, u64), String> {
    // vCPUs, the least and most memory in GiB, and the step in GiB.
    const SIZES: [(u32, u64, u64, u64); 5] = [
        (1, 2, 8, 1),
        (2, 4, 16, 1),
        (4, 8, 30, 1),
        (8, 16, 60, 4),
        (16, 32, 120, 8),
    ];
    let Some(&(fit, least, most, step)) = SIZES.iter().find(|size| size.0 >= cpus) else {
        return Err(format!(
            "task {task} asks for {cpus} CPUs, more than the 16 of the largest Fargate task"
        ));
    };
    if fit != cpus {
        tracing::warn!(task, cpus, fit, "Fargate rounds the CPUs of the task up");
    }
    let (least, most, step) = (least << 10, most << 10, step << 10);
    let memory = memory.unwrap_or(least);
    let fitted = match memory {
        memory if memory < least => least,
        memory if memory > most => most,
        memory => memory.div_ceil(step) * step,
    };
    if fitted != memory {
        tracing::warn!(
            task,
            memory,
            fitted,
            "Fargate moves the memory of the task (MiB)"
        );
    }
    Ok((fit, fitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_jobs() {
        assert_eq!(job_name("ALIGN (1)"), "ALIGN_1");
        assert_eq!(job_name("run-P-1-1"), "run-P-1-1");
        assert_eq!(job_name(&"x".repeat(300)).len(), 128);
    }

    #[test]
    fn sizes_fargate_tasks() {
        assert_eq!(fargate(1, None, "T"), Ok((1, 2048)));
        assert_eq!(fargate(3, Some(5000), "T"), Ok((4, 8192)));
        assert_eq!(fargate(2, Some(5000), "T"), Ok((2, 5120)));
        assert_eq!(fargate(8, Some(17 << 10), "T"), Ok((8, 20 << 10)));
        assert_eq!(fargate(16, Some(500 << 10), "T"), Ok((16, 120 << 10)));
        assert!(fargate(17, None, "T").is_err());
    }
}
