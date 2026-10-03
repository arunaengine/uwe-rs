//! `iunctus time <run>`: where the time of a run went. The critical path, the chain of tasks
//! that set the length of the run, with how long each waited and ran; how long all tasks ran
//! and waited together; and the steps that ran the longest and used the most CPU time, memory
//! and I/O.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use run_database::{StepTime, TimeSpent};

use super::{bytes, duration, json_format, print_json, run};
use crate::arguments::Arguments;
use crate::{Failure, failure, runs};

/// How many steps each list of the text names.
const LISTED: usize = 5;

pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(id) = arguments.target.as_ref().map(|id| id.to_string_lossy()) else {
        return Err(Failure::usage(
            "iunctus: name a run, like `iunctus time 1a0f1cc4c10-3a2487`; `iunctus runs` lists \
             the runs",
        ));
    };
    let json = json_format(arguments, "time")?;
    let database = runs::open(arguments)?;
    let record = run(&database, &id)?;
    let spent = database
        .time_spent(&id)
        .map_err(|error| failure(error.to_string()))?
        .ok_or_else(|| failure(format!("iunctus: the run database has no run `{id}`")))?;
    if json {
        return print_json(serde_json::to_value(&spent));
    }
    print!("{}", text(&spent, &record.workflow, record.simulated));
    Ok(())
}

/// A number of tasks, like `1 task` or `2 tasks`.
fn tasks(count: u64) -> String {
    match count {
        1 => "1 task".to_owned(),
        count => format!("{count} tasks"),
    }
}

/// Where the time went, in plain lines.
fn text(spent: &TimeSpent, workflow: &str, simulated: bool) -> String {
    let length = spent.ended - spent.started;
    let mut text = format!(
        "run {} of {workflow}: {}, {}",
        spent.run,
        duration(length),
        tasks(spent.tasks)
    );
    if spent.reused > 0 {
        let _ = write!(text, " ({} reused)", spent.reused);
    }
    text.push('\n');
    if simulated {
        text.push_str(
            "the run was simulated: the times are those of the simulator, not of the tools\n",
        );
    }
    let at_once = if length > 0 {
        spent.ran as f64 / length as f64
    } else {
        0.0
    };
    let _ = writeln!(
        text,
        "the tasks ran {} together, {at_once:.1} at a time on average, and waited {} to start",
        duration(spent.ran),
        duration(spent.waited)
    );
    let _ = writeln!(
        text,
        "no task ran for {} of the run",
        duration(length - spent.busy)
    );
    path(&mut text, spent);
    steps(&mut text, &spent.steps);
    text
}

/// The critical path, one line per part.
fn path(text: &mut String, spent: &TimeSpent) {
    let _ = writeln!(
        text,
        "\nthe critical path, the chain of tasks that set the length of the run:"
    );
    let _ = writeln!(
        text,
        "  {} before the first task: loading the workflow and planning",
        duration(spent.before)
    );
    for task in &spent.critical_path {
        let mut parts = Vec::new();
        if task.gap > 0 {
            parts.push(format!("{} after the task before it", duration(task.gap)));
        }
        parts.push(format!("waited {}", duration(task.waited)));
        match &task.reused {
            Some(source) if source == "cache" => parts.push("came from the cache".into()),
            Some(source) => parts.push(format!("came from run {source}")),
            None => parts.push(format!("ran {}", duration(task.ran))),
        }
        if task.attempts > 1 {
            parts.push(format!("{} attempts", task.attempts));
        }
        let _ = writeln!(
            text,
            "  task {} {}: {}",
            task.task,
            task.name,
            parts.join(", ")
        );
    }
    if !spent.critical_path.is_empty() {
        let _ = writeln!(
            text,
            "  {} after the last task: delivering results and reports",
            duration(spent.after)
        );
    }
}

/// The steps that ran the longest and used the most, a few of each.
fn steps(text: &mut String, steps: &[StepTime]) {
    let top = |key: &dyn Fn(&StepTime) -> Option<i64>| -> Vec<(&StepTime, i64)> {
        let mut found: Vec<(&StepTime, i64)> = (steps.iter())
            .filter_map(|step| Some((step, key(step).filter(|value| *value > 0)?)))
            .collect();
        found.sort_by_key(|(_, value)| std::cmp::Reverse(*value));
        found.truncate(LISTED);
        found
    };
    let width = steps.iter().map(|step| step.step.len()).max().unwrap_or(0);
    let mut list = |title: &str, found: Vec<(&StepTime, i64)>, show: &dyn Fn(i64) -> String| {
        if found.is_empty() {
            return;
        }
        let _ = writeln!(text, "\n{title}:");
        for (step, value) in found {
            let tasks = tasks(step.tasks);
            let _ = writeln!(
                text,
                "  {:<width$}  {:>12}  {tasks}",
                step.step,
                show(value)
            );
        }
    };
    list(
        "steps that ran the longest, all their tasks together",
        top(&|step| Some(step.ran)),
        &duration,
    );
    list(
        "steps that used the most CPU time",
        top(&|step| step.cpu_usec.map(|usec| usec / 1000)),
        &duration,
    );
    list(
        "steps that used the most memory, as the highest peak of one task",
        top(&|step| step.memory_peak),
        &bytes,
    );
    list(
        "steps that read and wrote the most",
        top(&|step| match (step.read_bytes, step.write_bytes) {
            (None, None) => None,
            (read, written) => Some(read.unwrap_or(0) + written.unwrap_or(0)),
        }),
        &bytes,
    );
    let measured = (steps.iter()).any(|step| step.cpu_usec.is_some() || step.memory_peak.is_some());
    if !measured && !steps.is_empty() {
        text.push_str(
            "\nno task has a measure of CPU time, memory or I/O: the local executor with Docker \
             or Podman measures them\n",
        );
    }
}

#[cfg(test)]
mod tests {
    use run_database::PathTask;

    use super::*;

    #[test]
    fn names_critical_steps() {
        let spent = TimeSpent {
            run: "r".into(),
            started: 0,
            ended: 10_000,
            tasks: 2,
            ran: 9_000,
            waited: 500,
            busy: 9_000,
            before: 500,
            after: 500,
            critical_path: vec![PathTask {
                task: 1,
                name: "ALIGN (a)".into(),
                step: "ALIGN".into(),
                waited: 0,
                ran: 9_000,
                attempts: 2,
                ..PathTask::default()
            }],
            steps: vec![StepTime {
                step: "ALIGN".into(),
                tasks: 2,
                ran: 9_000,
                cpu_usec: Some(20_000_000),
                memory_peak: Some(2_000_000_000),
                read_bytes: Some(1_000),
                ..StepTime::default()
            }],
            ..TimeSpent::default()
        };
        let text = text(&spent, "/w/main.nf", false);
        assert!(
            text.starts_with("run r of /w/main.nf: 10.0 s, 2 tasks\n"),
            "{text}"
        );
        assert!(text.contains("0.9 at a time on average"), "{text}");
        assert!(text.contains("no task ran for 1.0 s of the run"), "{text}");
        assert!(text.contains("  task 1 ALIGN (a): waited 0 ms, ran 9.0 s, 2 attempts\n"));
        assert!(text.contains("steps that used the most CPU time:\n  ALIGN        20.0 s"));
        assert!(text.contains("2.0 GB  2 tasks"), "{text}");
        assert!(!text.contains("no task has a measure"), "{text}");
        assert_eq!(tasks(1), "1 task");
    }
}
