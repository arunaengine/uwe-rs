//! Service jobs: a job with a `service()` output provides something, like a socket, that the
//! jobs that read it use while it runs. Snakemake runs such a job in one group with the jobs
//! that use it, all at once, and ends it with SIGTERM once they ended; the group succeeds when
//! they do. iunctus runs the group as one task in one container: the service starts first in
//! the background, each job that uses it starts once the service made its output, and the
//! service gets SIGTERM when they ended. The output of a service is no file that a run keeps
//! or checks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Input, Resources, Task, TaskId};

use crate::plan::Job;
use crate::shell::quote;

/// The `service()` outputs of a job.
pub(crate) fn service_files(job: &Job) -> Vec<String> {
    (job.output.values.iter())
        .filter(|value| value.tag("service").is_some())
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// The task of a service job and the jobs that use it, the service first, as one task with
/// the number `id`; an error says why they cannot share one container.
pub(crate) fn combine(members: &[Task], services: &[String], id: TaskId) -> Result<Task, String> {
    let (service, users) = members
        .split_first()
        .ok_or_else(|| "a service job needs its task".to_owned())?;
    if let Some(other) = users.iter().find(|task| task.image != service.image) {
        return Err(format!(
            "rule {} runs as a service of rule {}, in one container with it as Snakemake runs \
             them in one group, so both need the same container image, not {} and {}",
            service.step,
            other.step,
            service.image.as_deref().unwrap_or("the default image"),
            other.image.as_deref().unwrap_or("the default image"),
        ));
    }
    let is_service = |path: &str| services.iter().any(|file| file == path);
    let mut inputs: Vec<Input> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    let mut after: Vec<TaskId> = Vec::new();
    for task in members {
        for input in &task.inputs {
            if !is_service(&input.path) && !inputs.iter().any(|known| known.path == input.path) {
                inputs.push(input.clone());
            }
        }
        for output in &task.outputs {
            if !is_service(output) && !outputs.contains(output) {
                outputs.push(output.clone());
            }
        }
        for id in &task.after {
            if !after.contains(id) {
                after.push(*id);
            }
        }
    }
    let script = script(service, users, services);
    Ok(Task {
        id,
        name: members
            .iter()
            .map(|task| task.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        step: service.step.clone(),
        image: service.image.clone(),
        command: vec!["sh".into(), "-c".into(), script.clone()],
        script: Some(script),
        inputs,
        outputs,
        resources: resources(members),
        after,
        ..Task::default()
    })
}

/// The script of a group: the service in the background, then each job that uses it once the
/// service made its outputs, or ended. It ends with the first failure of these jobs, after it
/// sent SIGTERM to the service.
fn script(service: &Task, users: &[Task], services: &[String]) -> String {
    let mut script = format!("{} &\nservice=$!\n", subshell(service, ""));
    let made: Vec<String> = (services.iter())
        .map(|file| {
            let file = quote(file);
            format!("{{ [ -e {file} ] || [ -L {file} ]; }}")
        })
        .collect();
    let wait = format!(
        "while ! {{ {}; }} && kill -0 \"$service\" 2> /dev/null; do sleep 0.1; done; ",
        made.join(" && ")
    );
    for (number, task) in users.iter().enumerate() {
        script.push_str(&format!("{} &\nuser{number}=$!\n", subshell(task, &wait)));
    }
    script.push_str("status=0\n");
    for number in 0..users.len() {
        script.push_str(&format!(
            "wait \"$user{number}\" || {{ code=$?; [ \"$status\" -ne 0 ] || status=$code; }}\n"
        ));
    }
    script.push_str(
        "kill -TERM \"$service\" 2> /dev/null\nwait \"$service\" 2> /dev/null\nexit \"$status\"\n",
    );
    script
}

/// A subshell that runs the command of a task with its environment, after `before`.
fn subshell(task: &Task, before: &str) -> String {
    let exports: String = (task.environment.iter())
        .map(|(name, value)| format!("export {name}={}; ", quote(value)))
        .collect();
    let words: Vec<String> = task.command.iter().map(|word| quote(word)).collect();
    format!("( {before}{exports}exec {} )", words.join(" "))
}

/// What a group asks for: the CPUs and memory of all its jobs, which run at once, and the
/// longest time of one.
fn resources(members: &[Task]) -> Resources {
    let mut sum = Resources::default();
    for task in members {
        let own = &task.resources;
        sum.cpus = Some(sum.cpus.unwrap_or(0.0) + own.cpus.unwrap_or(1.0));
        sum.memory_bytes = match (sum.memory_bytes, own.memory_bytes) {
            (None, None) => None,
            (sum, own) => Some(sum.unwrap_or(0) + own.unwrap_or(0)),
        };
        sum.disk_bytes = sum.disk_bytes.max(own.disk_bytes);
        sum.time_seconds = sum.time_seconds.max(own.time_seconds);
        sum.gpus = sum.gpus.max(own.gpus);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(step: &str, command: &str, inputs: &[&str], outputs: &[&str]) -> Task {
        Task {
            step: step.to_owned(),
            name: step.to_owned(),
            command: vec!["bash".into(), "-c".into(), command.to_owned()],
            inputs: (inputs.iter())
                .map(|path| Input {
                    path: (*path).to_owned(),
                    ..Input::default()
                })
                .collect(),
            outputs: outputs.iter().map(|path| (*path).to_owned()).collect(),
            ..Task::default()
        }
    }

    #[test]
    fn a_service_and_its_users_run_as_one_task() {
        let service = task(
            "a",
            "ln -s /dev/random foo.socket; sleep 10000",
            &[],
            &["foo.socket"],
        );
        let first = task(
            "b",
            "head -n1 foo.socket > test.txt",
            &["foo.socket"],
            &["test.txt"],
        );
        let second = task(
            "c",
            "head -n1 foo.socket > test2.txt",
            &["foo.socket"],
            &["test2.txt"],
        );
        let services = ["foo.socket".to_owned()];
        let group = combine(&[service, first, second], &services, TaskId(7)).expect("a task");
        assert_eq!(group.id, TaskId(7));
        assert!(group.inputs.is_empty());
        assert_eq!(group.outputs, ["test.txt", "test2.txt"]);
        assert_eq!(group.resources.cpus, Some(3.0));
        let script = &group.command[2];
        assert!(
            script.starts_with(
                "( exec bash -c 'ln -s /dev/random foo.socket; sleep 10000' ) &\nservice=$!\n"
            ),
            "{script}"
        );
        assert!(
            script.contains("while ! { { [ -e foo.socket ] || [ -L foo.socket ]; }; }"),
            "{script}"
        );
        assert!(script.ends_with("kill -TERM \"$service\" 2> /dev/null\nwait \"$service\" 2> /dev/null\nexit \"$status\"\n"), "{script}");
    }

    #[test]
    fn a_service_needs_the_image_of_its_users() {
        let service = task("a", "true", &[], &["s"]);
        let mut user = task("b", "true", &["s"], &[]);
        user.image = Some("other".into());
        let error = combine(&[service, user], &["s".to_owned()], TaskId(1)).unwrap_err();
        assert!(error.contains("same container image"), "{error}");
    }
}
