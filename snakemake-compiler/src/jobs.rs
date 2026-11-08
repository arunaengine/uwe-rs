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
pub(crate) fn graph(
    jobs: &[PlannedJob],
    setting: &Setting,
    launch: &Launch,
    targets: &Json,
) -> Json {
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
        let mut builder = json!({"task": task, "layout": crate::launch::layout(jobs, index)});
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
    let mut outputs = Vec::new();
    for (name, value) in targets.as_object().into_iter().flatten() {
        let Some((op, edge, port)) = result(jobs, &paths, name, value) else {
            continue;
        };
        ops.push(op);
        edges.push(edge);
        outputs.push(port);
    }
    json!({"name": "main", "outputs": outputs, "ops": ops, "edges": edges})
}

/// The output `name` of the entry graph with the target files of `value`, as a native run
/// names them: an expr op that gives them once the ops that make them have ended.
fn result(
    jobs: &[PlannedJob],
    paths: &[String],
    name: &str,
    value: &Json,
) -> Option<(Json, Json, Json)> {
    let files: Vec<&Json> = match value {
        Json::Array(items) => items.iter().collect(),
        file => vec![file],
    };
    let mut locations = Vec::new();
    let mut after = Vec::new();
    for file in &files {
        let location = file["location"].as_str()?;
        // The text of a WDL string takes placeholders, which a path rarely holds.
        if location.contains("~{") || location.contains("${") {
            return None;
        }
        locations.push(serde_json::to_string(location).ok()?);
        let made = (jobs.iter().enumerate())
            .flat_map(|(index, job)| {
                job.outputs
                    .iter()
                    .map(move |made| (index, made.path.trim_end_matches('/')))
            })
            .filter(|(_, path)| location == *path || location.ends_with(&format!("/{path}")))
            .max_by_key(|(_, path)| path.len());
        if let Some((index, _)) = made
            && !after
                .iter()
                .any(|before: &Json| before["op"] == paths[index])
        {
            after.push(json!({"op": paths[index], "on": ["succeeded"]}));
        }
    }
    let class = |file: &Json| match file["class"] == "Directory" {
        true => json!("directory"),
        false => json!({"file": {}}),
    };
    let classes: Vec<Json> = files.iter().map(|file| class(file)).collect();
    let mixed = classes.windows(2).any(|pair| pair[0] != pair[1]);
    let mut library = Vec::new();
    let (code, kind) = match value {
        // Files and folders in one list are typed one by one, the list as either.
        Json::Array(_) if mixed => {
            let names: Vec<String> = (0..files.len())
                .map(|index| format!("item_{index}"))
                .collect();
            for ((name, location), class) in names.iter().zip(&locations).zip(&classes) {
                let declared = if *class == json!("directory") {
                    "Directory"
                } else {
                    "File"
                };
                library.push(format!("{declared} {name} = {location}"));
            }
            let items = json!({"union": [{"file": {}}, "directory"]});
            (
                format!("[{}]", names.join(", ")),
                json!({"array": {"items": items}}),
            )
        }
        Json::Array(_) => {
            let items = classes.first().cloned().unwrap_or(json!({"file": {}}));
            (
                format!("[{}]", locations.join(", ")),
                json!({"array": {"items": items}}),
            )
        }
        file => (locations.join(""), class(file)),
    };
    let path = format!("result.{name}");
    let mut expr = json!({"language": "wdl", "code": code});
    if !library.is_empty() {
        expr["library"] = json!(library);
    }
    let mut op = json!({"path": path, "kind": {"expr": expr},
        "outputs": [{"name": "value", "type": kind}]});
    if !after.is_empty() {
        op["after"] = json!(after);
    }
    let edge = json!({"from": {"op": path, "port": "value"}, "to": {"port": name}});
    Some((op, edge, json!({"name": name, "type": kind})))
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
