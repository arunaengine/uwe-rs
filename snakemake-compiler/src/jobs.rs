//! The graph of a planned workflow: a dialect task op per job at the path of its rule and
//! wildcards, and an edge with a File per file that one job makes and another reads.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use serde_json::{Value as Json, json};
use snakemake_runtime::{JobFile, PlannedJob};
use workflow_graph::Launch;

use crate::{SOURCE, Setting};

/// The flags of files that the native run does not deliver to the working directory.
const UNDELIVERED: [&str; 3] = ["temp", "pipe", "service"];

/// The entry graph `main` of the jobs, in the order of the plan.
pub(crate) fn graph(jobs: &[PlannedJob], setting: &Setting, launch: &Launch) -> Json {
    let paths = paths(jobs);
    let plan = plan(setting, launch);
    let mut ops = Vec::new();
    let mut edges = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        let mut inputs = Vec::new();
        let mut maps = Vec::new();
        for producer in &job.producers {
            let port = format!("input_{}", producer.input);
            let made = &jobs[producer.job].outputs[producer.output];
            inputs.push(json!({"name": port, "type": file_type(made)}));
            maps.push(json!({"port": port, "native": format!("input/{}", producer.input)}));
            edges.push(json!({
                "from": {"op": paths[producer.job], "port": format!("output_{}", producer.output)},
                "to": {"op": paths[index], "port": port},
            }));
        }
        let outputs: Vec<Json> = (job.outputs.iter().enumerate())
            .map(|(position, file)| json!({"name": format!("output_{position}"), "type": file_type(file)}))
            .collect();
        let output_maps: Vec<Json> = (0..job.outputs.len())
            .map(|position| json!({"port": format!("output_{position}"), "native": format!("output/{position}")}))
            .collect();
        let effects: Vec<Json> = (job.outputs.iter())
            .filter(|file| {
                !file
                    .flags
                    .iter()
                    .any(|flag| UNDELIVERED.contains(&flag.as_str()))
            })
            .map(|file| json!({"publish": file.path}))
            .collect();
        let rule = &job.key.rule;
        let mut task = json!({
            "source": SOURCE,
            "entry": setting.entry.clone().unwrap_or_else(|| rule.clone()),
            "call": rule,
        });
        for (field, value) in [("inputs", maps), ("outputs", output_maps)] {
            if !value.is_empty() {
                task[field] = json!(value);
            }
        }
        if !setting.profiles.is_empty() {
            task["profiles"] = json!(setting.profiles);
        }
        if !setting.configs.is_empty() {
            task["configs"] = json!(setting.configs);
        }
        let mut builder = json!({"task": task});
        if !job.key.wildcards.is_empty() {
            builder["wildcards"] = json!(job.key.wildcards);
        }
        if !job.group.is_empty() {
            let members = (job.group.iter()).map(|member| {
                json!({"source": SOURCE, "rule": member.rule, "wildcards": member.wildcards})
            });
            builder["group"] = members.collect();
        }
        for (field, value) in &plan {
            builder[*field] = value.clone();
        }
        let mut op = json!({
            "path": paths[index],
            "kind": {"task": {"builder": {"snakemake_job": builder}}},
            "origin": {"source": SOURCE, "at": rule},
        });
        let after: Vec<Json> = (job.after.iter())
            .map(|before| json!({"op": paths[*before], "on": ["succeeded"]}))
            .collect();
        for (field, value) in [
            ("inputs", inputs),
            ("outputs", outputs),
            ("after", after),
            ("effects", effects),
        ] {
            if !value.is_empty() {
                op[field] = json!(value);
            }
        }
        ops.push(op);
    }
    json!({"name": "main", "ops": ops, "edges": edges})
}

/// The fields of the plan settings of a launch that are set, as every job carries them.
fn plan(setting: &Setting, launch: &Launch) -> Vec<(&'static str, Json)> {
    let lists = [
        ("targets", &launch.targets),
        ("until", &launch.until),
        ("omit_from", &launch.omit_from),
        ("forcerun", &launch.forcerun),
    ];
    let mut plan: Vec<(&str, Json)> = (lists.into_iter())
        .filter(|(field, list)| list.len() > usize::from(*field == "targets"))
        .map(|(field, list)| (field, json!(list)))
        .collect();
    if let Some(values) = &setting.values {
        plan.push(("config_values", json!(values)));
    }
    if launch.forceall {
        plan.push(("forceall", json!(true)));
    }
    plan
}

fn file_type(file: &JobFile) -> Json {
    match file.flags.iter().any(|flag| flag == "directory") {
        true => json!("directory"),
        false => json!({"file": {}}),
    }
}

/// The op path of each job: its rule, then its wildcard values, with letters that names do
/// not take as `_`; a path that two jobs would share gets a number.
fn paths(jobs: &[PlannedJob]) -> Vec<String> {
    let mut seen = HashSet::new();
    (jobs.iter())
        .map(|job| {
            let mut path = name(&job.key.rule);
            if !job.key.wildcards.is_empty() {
                let values: Vec<String> = job.key.wildcards.values().map(|v| name(v)).collect();
                path = format!("{path}/{}", values.join("."));
            }
            let mut unique = path.clone();
            let mut number = 1;
            while !seen.insert(unique.clone()) {
                number += 1;
                unique = format!("{path}-{number}");
            }
            unique
        })
        .collect()
}

fn name(text: &str) -> String {
    let named: String = (text.chars())
        .map(
            |letter| match letter.is_ascii_alphanumeric() || "_-.".contains(letter) {
                true => letter,
                false => '_',
            },
        )
        .collect();
    match named.is_empty() {
        true => "_".to_owned(),
        false => named,
    }
}
