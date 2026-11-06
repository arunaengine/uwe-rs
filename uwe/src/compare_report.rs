//! The report of `uwe compare -report`: the layers of nf-diff over uwe's own run records.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};

use run_database::{Comparison, Database, Pairing, Run, TaskDetail, TaskPair};
use serde_json::{Value as Json, json};

/// The details of the tasks of both runs, by task number.
#[derive(Default)]
pub(super) struct Details {
    pub(super) first: HashMap<u64, TaskDetail>,
    pub(super) second: HashMap<u64, TaskDetail>,
}

impl Details {
    /// The details of every task of the comparison.
    pub(super) fn load(
        database: &Database,
        comparison: &Comparison,
    ) -> Result<Details, run_database::DatabaseError> {
        let mut details = Details::default();
        for pair in &comparison.tasks {
            if let Some(side) = &pair.first
                && let Some(task) = database.task(&comparison.first.id, side.task)?
            {
                details.first.insert(side.task, task);
            }
            if let Some(side) = &pair.second
                && let Some(task) = database.task(&comparison.second.id, side.task)?
            {
                details.second.insert(side.task, task);
            }
        }
        Ok(details)
    }
}

/// A task of a pair with its details, in one of the runs.
struct Sided<'a> {
    name: &'a str,
    state: &'a str,
    exit: Option<i32>,
    image: Option<&'a str>,
    detail: Option<&'a TaskDetail>,
}

fn sides<'a>(pair: &'a TaskPair, details: &'a Details) -> [Option<Sided<'a>>; 2] {
    let sided = |side: &'a Option<run_database::Side>, map: &'a HashMap<u64, TaskDetail>| {
        side.as_ref().map(|side| Sided {
            name: &side.name,
            state: &side.state,
            exit: side.exit_code,
            image: side.image.as_deref(),
            detail: map.get(&side.task),
        })
    };
    [
        sided(&pair.first, &details.first),
        sided(&pair.second, &details.second),
    ]
}

/// The report as JSON, with the top-level keys of nf-diff's report.
pub(super) fn report(comparison: &Comparison, details: &Details, generated: i64) -> Json {
    let tasks = task_rows(comparison, details);
    let count = |kind: &str| tasks.iter().filter(|task| task["kind"] == kind).count();
    let unchanged = (comparison.tasks.iter())
        .filter(|pair| pair.first.is_some() && pair.second.is_some())
        .count()
        - count("changed");
    let recomputed = (comparison.tasks.iter())
        .filter(|pair| pair.pairing == Pairing::Changed)
        .count();
    let identical = count("changed") + count("added") + count("removed") == 0;
    json!({
        "schemaVersion": "1",
        "generatedAt": utc(generated, true),
        "identical": identical,
        "warnings": [],
        "showObvious": false,
        "runA": run(&comparison.first, cached(comparison, 0)),
        "runB": run(&comparison.second, cached(comparison, 1)),
        "summary": {
            "tasksChanged": count("changed"),
            "tasksAdded": count("added"),
            "tasksRemoved": count("removed"),
            "tasksUnchanged": unchanged,
            "tasksRecomputed": recomputed,
            "publishedChanged": 0,
            "logsChanged": 0,
            "dagEdgesAdded": 0,
            "dagEdgesRemoved": 0,
        },
        "metadata": metadata(&comparison.first, &comparison.second),
        "params": params(&comparison.first, &comparison.second),
        "config": [],
        "configNote": "uwe records no resolved configuration of a run; the configuration is not compared.",
        "configProvenance": null,
        "processes": processes(comparison),
        "tasks": tasks.into_iter().filter(|task| task["kind"] != "unchanged").collect::<Vec<_>>(),
        "diffPublished": false,
        "publishedNote": null,
        "published": null,
        "diffLogs": false,
        "logsNote": null,
        "logs": [],
        "diffDag": false,
        "dagNote": null,
        "dag": [],
    })
}

/// A time in UTC, ISO 8601 with `Z`, with or without milliseconds.
fn utc(milliseconds: i64, fraction: bool) -> String {
    let format = if fraction {
        "%Y-%m-%dT%H:%M:%S%.3fZ"
    } else {
        "%Y-%m-%dT%H:%M:%SZ"
    };
    jiff::Timestamp::from_millisecond(milliseconds)
        .map(|time| time.strftime(format).to_string())
        .unwrap_or_default()
}

/// `OK` for a run that succeeded, else `ERR`, as Nextflow's history says.
fn status(run: &Run) -> &'static str {
    if run.state == "succeeded" {
        "OK"
    } else {
        "ERR"
    }
}

fn command(run: &Run) -> String {
    let mut parts = vec!["uwe run".to_owned(), run.workflow.clone()];
    parts.extend(run.arguments.iter().cloned());
    parts.join(" ")
}

/// The tasks of one run of the comparison whose result came from the cache or another run.
fn cached(comparison: &Comparison, index: usize) -> usize {
    let sides = comparison
        .tasks
        .iter()
        .map(|pair| [&pair.first, &pair.second][index]);
    sides.flatten().filter(|side| side.reused.is_some()).count()
}

fn run(run: &Run, reused: usize) -> Json {
    json!({
        "runName": run.id,
        "sessionId": run.lineage,
        "status": status(run),
        "revision": run.revision,
        "command": command(run),
        "launched": utc(run.started, false),
        "wallDurationMillis": run.ended.map(|ended| ended - run.started),
        "taskCount": run.tasks.values().sum::<u64>(),
        "cachedTasks": reused,
    })
}

/// A field of both runs; `obvious` for fields that differ between any two runs.
fn field(name: &str, a: Option<String>, b: Option<String>, obvious: bool) -> Json {
    let changed = a != b;
    json!({
        "field": name,
        "valueA": a,
        "valueB": b,
        "changed": changed,
        "obvious": obvious,
        "highlighted": changed && !obvious,
    })
}

fn metadata(a: &Run, b: &Run) -> Vec<Json> {
    let state = |run: &Run| {
        Some(
            if status(run) == "OK" {
                "SUCCEEDED"
            } else {
                "FAILED"
            }
            .to_owned(),
        )
    };
    vec![
        field(
            "Pipeline",
            Some(a.workflow.clone()),
            Some(b.workflow.clone()),
            false,
        ),
        field("Run name", Some(a.id.clone()), Some(b.id.clone()), true),
        field("Status", state(a), state(b), false),
        field("Revision", a.revision.clone(), b.revision.clone(), false),
        field("Command", Some(command(a)), Some(command(b)), true),
        field(
            "Language",
            Some(a.language.clone()),
            Some(b.language.clone()),
            false,
        ),
        field(
            "Executor",
            Some(a.executor.clone()),
            Some(b.executor.clone()),
            false,
        ),
        field("Program", a.program.clone(), b.program.clone(), false),
    ]
}

/// The workflow parameters of both runs, as given on the command line.
fn params(a: &Run, b: &Run) -> Vec<Json> {
    let values =
        |run: &Run| -> BTreeMap<String, String> { run.parameters.iter().cloned().collect() };
    let (a, b) = (values(a), values(b));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    names
        .into_iter()
        .map(|name| {
            let mut row = field(
                &format!("--{name}"),
                a.get(name).cloned(),
                b.get(name).cloned(),
                false,
            );
            for (key, side) in [("sourceA", &a), ("sourceB", &b)] {
                if side.contains_key(name) {
                    row[key] = json!("CLI");
                }
            }
            row
        })
        .collect()
}

/// The number of tasks of each step in both runs.
fn processes(comparison: &Comparison) -> Vec<Json> {
    let mut counts: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for pair in &comparison.tasks {
        let entry = counts.entry(&pair.step).or_default();
        entry.0 += u64::from(pair.first.is_some());
        entry.1 += u64::from(pair.second.is_some());
    }
    (counts.into_iter())
        .map(|(process, (a, b))| {
            let status = match (a, b) {
                (0, _) => "added",
                (_, 0) => "removed",
                (a, b) if a != b => "changed",
                _ => "unchanged",
            };
            json!({ "process": process, "countA": a, "countB": b, "status": status })
        })
        .collect()
}

fn image(side: &Sided) -> Option<String> {
    let named = side
        .detail
        .and_then(|detail| detail.description.image.as_deref());
    side.image.or(named).map(str::to_owned)
}

/// Each task of both runs with the fields that differ; `kind` says how it compares.
fn task_rows(comparison: &Comparison, details: &Details) -> Vec<Json> {
    let mut rows = Vec::new();
    for pair in &comparison.tasks {
        let [a, b] = sides(pair, details);
        let (kind, key, fields) = match (&a, &b) {
            (Some(a), Some(b)) => {
                let fields: Vec<Json> = task_fields(a, b)
                    .into_iter()
                    .filter(|row| row["changed"] == true)
                    .collect();
                let changed = pair.pairing == Pairing::Changed || !fields.is_empty();
                (
                    if changed { "changed" } else { "unchanged" },
                    b.name,
                    fields,
                )
            }
            (Some(a), None) => ("removed", a.name, Vec::new()),
            (None, Some(b)) => ("added", b.name, Vec::new()),
            (None, None) => continue,
        };
        rows.push(json!({
            "key": key,
            "kind": kind,
            "process": pair.step,
            "fields": fields,
            "notes": pair.changes,
        }));
    }
    rows
}

fn task_fields(a: &Sided, b: &Sided) -> Vec<Json> {
    let script = |side: &Sided| {
        let detail = side.detail?;
        let task = &detail.description;
        Some(
            task.script
                .clone()
                .unwrap_or_else(|| task.command.join(" ")),
        )
    };
    let cpus = |side: &Sided| Some(side.detail?.description.resources.cpus?.to_string());
    let memory = |side: &Sided| Some(side.detail?.description.resources.memory_bytes?.to_string());
    let exit = |side: &Sided| side.exit.map(|code| code.to_string());
    vec![
        field(
            "status",
            Some(a.state.to_owned()),
            Some(b.state.to_owned()),
            false,
        ),
        field("exit", exit(a), exit(b), false),
        field("container", image(a), image(b), false),
        field("script", script(a), script(b), false),
        field("cpus", cpus(a), cpus(b), false),
        field("memory", memory(a), memory(b), false),
    ]
}
