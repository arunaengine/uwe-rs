//! The plans of a Snakemake region with checkpoints: the jobs that can be planned now as an
//! update of its graph, until no job waits for a checkpoint.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use serde_json::{Value as Json, json};
use snakemake_runtime::PlannedJob;
use wir::RuleTask;
use wir::reserved::PlanUpdate;
use workflow_graph::Launch;

use crate::{SOURCE, Setting, jobs};

/// The plan update `revision` of a region whose jobs the planner of `rule` planned: the jobs
/// that wait for no checkpoint, final once none waits.
pub fn region_plan(
    jobs: &[PlannedJob],
    rule: &RuleTask,
    revision: u64,
) -> Result<PlanUpdate, String> {
    // A job waits when a checkpoint decides its inputs, or when it reads a waiting job that
    // runs; one that does not run leaves its files where they are.
    let mut waiting = vec![false; jobs.len()];
    let mut moved = true;
    while moved {
        moved = false;
        for (index, job) in jobs.iter().enumerate() {
            let feeding = (job.producers.iter().map(|producer| producer.job))
                .chain(job.after.iter().copied());
            let waits = job.native.as_deref() == Some(snakemake_runtime::WAITING)
                || feeding
                    .into_iter()
                    .any(|other| waiting[other] && jobs[other].needed);
            if waits && !waiting[index] {
                waiting[index] = true;
                moved = true;
            }
        }
    }
    let kept: Vec<usize> = (0..jobs.len()).filter(|index| !waiting[*index]).collect();
    let position: HashMap<usize, usize> = (kept.iter().enumerate())
        .map(|(position, index)| (*index, position))
        .collect();
    let planned: Vec<PlannedJob> = (kept.iter())
        .map(|index| {
            let mut job = jobs[*index].clone();
            job.producers
                .retain(|producer| position.contains_key(&producer.job));
            for producer in &mut job.producers {
                producer.job = position[&producer.job];
            }
            job.after = (job.after.iter())
                .filter_map(|after| position.get(after).copied())
                .collect();
            job
        })
        .collect();
    let reference = &rule.task;
    let setting = Setting {
        entry: Some(reference.entry.clone()),
        profiles: reference.profiles.clone(),
        configs: reference.configs.clone(),
        values: rule.config_values.clone(),
    };
    let launch = crate::plan_launch(rule, Vec::new(), Launch::default());
    let mut graph = jobs::graph(&planned, &setting, &launch, &Json::Null);
    graph["name"] = json!("plan");
    // A run checks the layout of each job against the whole plan.
    let ops = graph["ops"].as_array_mut().into_iter().flatten();
    for (op, index) in ops.zip(&kept) {
        let builder = &mut op["kind"]["task"]["builder"]["snakemake_job"];
        builder["layout"] = json!(crate::launch::layout(jobs, *index));
    }
    let keys: Vec<Json> = (graph["ops"].as_array().into_iter().flatten())
        .zip(&planned)
        .map(|(op, job)| {
            let identity =
                json!({"source": SOURCE, "rule": job.key.rule, "wildcards": job.key.wildcards});
            json!({"op": op["path"], "identity": {"rule": identity}})
        })
        .collect();
    let update = json!({"revision": revision, "graph": graph, "keys": keys,
        "final_plan": !waiting.contains(&true)});
    serde_json::from_value(update).map_err(|error| error.to_string())
}
