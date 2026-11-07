//! The report of `uwe compare -report`: the layers of nf-diff over uwe's own run records.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap};

use run_database::{Comparison, Database, Pairing, Run, TaskDetail, TaskPair};
use serde_json::{Value as Json, json};

/// A task metric must change by this many percent to count as a regression.
const THRESHOLD: f64 = 25.0;

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
    let software = software(comparison, details);
    let software_changed = software
        .iter()
        .filter(|row| row["status"] == "changed")
        .count();
    let outputs = outputs(comparison, details);
    let outputs_changed: usize = (outputs.iter())
        .map(|task| task["files"].as_array().map_or(0, Vec::len))
        .sum();
    let regressions = regressions(comparison, details);
    let worse = (regressions.iter())
        .filter(|row| row["improved"] == false)
        .count();
    let efficiency = efficiency(comparison, details);
    let classes = |key: &str, class: &str| {
        let rows = efficiency.iter();
        rows.filter(|row| {
            row[format!("cpuClass{key}")] == class || row[format!("memClass{key}")] == class
        })
        .count()
    };
    let failures = failures(comparison);
    let failed = |side: &str| {
        failures[format!("tasks{side}")]
            .as_array()
            .map_or(0, Vec::len)
    };
    let groups = failures["groups"].as_array().cloned().unwrap_or_default();
    let grouped = |status: &str| {
        groups
            .iter()
            .filter(|group| group["status"] == status)
            .count()
    };
    let identical =
        count("changed") + count("added") + count("removed") + software_changed + outputs_changed
            == 0;
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
            "regressions": worse,
            "softwareChanged": software_changed,
            "outputsChanged": outputs_changed,
            "publishedChanged": 0,
            "logsChanged": 0,
            "overProvisionedA": classes("A", "over-provisioned"),
            "overProvisionedB": classes("B", "over-provisioned"),
            "tightA": classes("A", "tight"),
            "tightB": classes("B", "tight"),
            "failedA": failed("A"),
            "failedB": failed("B"),
            "newFailures": grouped("new"),
            "resolvedFailures": grouped("resolved"),
            "dagEdgesAdded": 0,
            "dagEdgesRemoved": 0,
        },
        "metadata": metadata(&comparison.first, &comparison.second),
        "params": params(&comparison.first, &comparison.second),
        "config": [],
        "configNote": "uwe records no resolved configuration of a run; the configuration is not compared.",
        "configProvenance": null,
        "processes": processes(comparison),
        "software": software,
        "efficiency": efficiency,
        "failures": failures,
        "tasks": tasks.into_iter().filter(|task| task["kind"] != "unchanged").collect::<Vec<_>>(),
        "perfThreshold": THRESHOLD,
        "regressions": regressions,
        "diffOutputs": true,
        "outputsNote": "Output files compared by size and digest, as the run database recorded them.",
        "outputs": outputs,
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

/// The images that the tasks of each step ran in, in both runs.
fn software(comparison: &Comparison, details: &Details) -> Vec<Json> {
    let mut images: BTreeMap<&str, [BTreeSet<String>; 2]> = BTreeMap::new();
    for pair in &comparison.tasks {
        let sets = images.entry(&pair.step).or_default();
        for (index, side) in sides(pair, details).into_iter().enumerate() {
            let image = side.and_then(|side| image(&side));
            sets[index].extend(image);
        }
    }
    (images.into_iter())
        .map(|(process, [a, b])| {
            let changed = a != b;
            json!({
                "process": process,
                "status": if changed { "changed" } else { "unchanged" },
                "containerChanged": changed,
                "condaChanged": false,
                "containersA": a,
                "containersB": b,
                "condaA": [],
                "condaB": [],
            })
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

/// What the last attempt of a task measured: wall time in milliseconds, CPU time in
/// microseconds and the peak of memory in bytes.
fn measured(side: &Sided) -> Option<(i64, Option<i64>, Option<i64>)> {
    let attempt = side.detail?.attempts.last()?;
    let wall = attempt.ended? - attempt.started;
    Some((wall, attempt.cpu_usec, attempt.memory_peak))
}

/// Matched tasks whose wall time or memory peak changed by at least [`THRESHOLD`] percent,
/// the largest change first.
fn regressions(comparison: &Comparison, details: &Details) -> Vec<Json> {
    let mut rows = Vec::new();
    for pair in &comparison.tasks {
        let [Some(a), Some(b)] = sides(pair, details) else {
            continue;
        };
        let (Some(first), Some(second)) = (measured(&a), measured(&b)) else {
            continue;
        };
        let same = a.detail.map(|detail| &detail.fingerprint)
            == b.detail.map(|detail| &detail.fingerprint);
        for (metric, x, y) in [
            ("realtime", Some(first.0), Some(second.0)),
            ("peak_rss", first.2, second.2),
        ] {
            let (Some(x), Some(y)) = (x, y) else { continue };
            if x <= 0 {
                continue;
            }
            let delta = (y - x) as f64 * 100.0 / x as f64;
            if delta.abs() < THRESHOLD {
                continue;
            }
            rows.push(json!({
                "key": b.name, "process": pair.step, "metric": metric, "valueA": x, "valueB": y,
                "deltaPct": (delta * 10.0).round() / 10.0, "improved": delta < 0.0,
                "sameWork": same,
            }));
        }
    }
    rows.sort_by(|x, y| {
        let size = |row: &Json| row["deltaPct"].as_f64().unwrap_or(0.0).abs();
        size(y).total_cmp(&size(x))
    });
    rows
}

/// The peak use of each step against what its tasks asked for, in both runs.
fn efficiency(comparison: &Comparison, details: &Details) -> Vec<Json> {
    // Per step and run: requested CPUs, peak CPU percent, requested and peak memory.
    type Use = (Option<f64>, Option<f64>, Option<u64>, Option<i64>);
    let mut steps: BTreeMap<&str, [Use; 2]> = BTreeMap::new();
    for pair in &comparison.tasks {
        let entry = steps.entry(&pair.step).or_default();
        for (index, side) in sides(pair, details).into_iter().enumerate() {
            let Some(side) = side else { continue };
            let resources = side.detail.map(|detail| &detail.description.resources);
            let slot = &mut entry[index];
            slot.0 = max(slot.0, resources.and_then(|resources| resources.cpus));
            slot.2 = slot
                .2
                .max(resources.and_then(|resources| resources.memory_bytes));
            if let Some((wall, cpu, memory)) = measured(&side) {
                let percent = cpu
                    .filter(|_| wall > 0)
                    .map(|cpu| cpu as f64 / wall as f64 / 10.0);
                slot.1 = max(slot.1, percent);
                slot.3 = slot.3.max(memory);
            }
        }
    }
    let class = |ratio: Option<f64>| {
        ratio.map(|ratio| match ratio {
            ratio if ratio < 0.5 => "over-provisioned",
            ratio if ratio >= 0.9 => "tight",
            _ => "right-sized",
        })
    };
    (steps.into_iter())
        .map(|(process, [a, b])| {
            let cpu = |side: &Use| Some(side.1? / 100.0 / side.0.filter(|cpus| *cpus > 0.0)?);
            let memory =
                |side: &Use| Some(side.3? as f64 / side.2.filter(|bytes| *bytes > 0)? as f64);
            json!({
                "process": process,
                "cpusRequestedA": a.0, "cpusRequestedB": b.0,
                "peakCpuPctA": a.1, "peakCpuPctB": b.1,
                "cpuEfficiencyA": cpu(&a), "cpuEfficiencyB": cpu(&b),
                "cpuClassA": class(cpu(&a)), "cpuClassB": class(cpu(&b)),
                "memRequestedBytesA": a.2, "memRequestedBytesB": b.2,
                "peakRssBytesA": a.3, "peakRssBytesB": b.3,
                "memEfficiencyA": memory(&a), "memEfficiencyB": memory(&b),
                "memClassA": class(memory(&a)), "memClassB": class(memory(&b)),
            })
        })
        .collect()
}

fn max(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The failed tasks of both runs, grouped by step, state and exit code: groups only in the
/// second run are new, only in the first resolved, in both persistent.
fn failures(comparison: &Comparison) -> Json {
    let mut groups: BTreeMap<(String, String, Option<i32>), [u64; 2]> = BTreeMap::new();
    let mut tasks: [Vec<Json>; 2] = Default::default();
    for pair in &comparison.tasks {
        for (index, side) in [&pair.first, &pair.second].into_iter().enumerate() {
            let Some(side) = side else { continue };
            let failed = side.state == "failed" || side.exit_code.is_some_and(|code| code != 0);
            if !failed {
                continue;
            }
            let key = (pair.step.clone(), side.state.clone(), side.exit_code);
            groups.entry(key).or_default()[index] += 1;
            tasks[index].push(json!({ "key": side.name, "process": pair.step,
                "status": side.state, "exit": side.exit_code }));
        }
    }
    let mut groups: Vec<Json> = (groups.into_iter())
        .map(|((process, state, exit), [a, b])| {
            let status = match (a, b) {
                (0, _) => "new",
                (_, 0) => "resolved",
                _ => "persistent",
            };
            json!({ "process": process, "taskStatus": state, "exit": exit, "countA": a,
                "countB": b, "status": status })
        })
        .collect();
    let size = |group: &Json| {
        group["countA"].as_u64().unwrap_or(0) + group["countB"].as_u64().unwrap_or(0)
    };
    groups.sort_by_key(|group| std::cmp::Reverse(size(group)));
    let [a, b] = tasks;
    json!({
        "runFailedA": status(&comparison.first) == "ERR",
        "runFailedB": status(&comparison.second) == "ERR",
        "groups": groups,
        "tasksA": a,
        "tasksB": b,
    })
}

/// The output files of matched tasks that differ, by their path in the task.
fn outputs(comparison: &Comparison, details: &Details) -> Vec<Json> {
    let mut rows = Vec::new();
    for pair in &comparison.tasks {
        let [Some(a), Some(b)] = sides(pair, details) else {
            continue;
        };
        let files = |side: &Sided| -> BTreeMap<String, (Option<u64>, Option<String>)> {
            let outputs = side
                .detail
                .map(|detail| detail.outputs.as_slice())
                .unwrap_or_default();
            (outputs.iter())
                .map(|file| (file.path.clone(), (file.size, file.digest.clone())))
                .collect()
        };
        let (first, second) = (files(&a), files(&b));
        let names: BTreeSet<&String> = first.keys().chain(second.keys()).collect();
        let changed: Vec<Json> = (names.into_iter())
            .filter_map(|name| {
                let (x, y) = (first.get(name), second.get(name));
                let status = match (x, y) {
                    (Some(x), Some(y)) if x == y => return None,
                    (Some(_), Some(_)) => "changed",
                    (Some(_), None) => "removed",
                    _ => "added",
                };
                Some(json!({
                    "file": name, "sizeA": x.and_then(|x| x.0), "sizeB": y.and_then(|y| y.0),
                    "digestA": x.and_then(|x| x.1.clone()), "digestB": y.and_then(|y| y.1.clone()),
                    "status": status,
                }))
            })
            .collect();
        if !changed.is_empty() {
            rows.push(json!({ "key": b.name, "process": pair.step, "files": changed }));
        }
    }
    rows
}

/// A part of a page of the report: a heading, an optional note, and what it shows.
struct Section {
    id: &'static str,
    title: &'static str,
    note: Option<String>,
    blocks: Vec<Block>,
}

enum Block {
    Table(Vec<&'static str>, Vec<Vec<String>>),
    Heading(String),
    Items(Vec<String>),
    Text(String),
}

/// The report as Markdown, with the sections of nf-diff's Markdown report.
pub(super) fn markdown(report: &Json) -> String {
    let mut out = format!(
        "# Run comparison report\n\n> Generated {} · meaningful changes only\n",
        text(&report["generatedAt"])
    );
    for section in sections(report) {
        out.push_str(&format!("\n## {}\n\n", section.title));
        if let Some(note) = &section.note {
            out.push_str(&format!("> {note}\n\n"));
        }
        for block in &section.blocks {
            match block {
                Block::Table(headers, rows) => {
                    out.push_str(&format!("| {} |\n", headers.join(" | ")));
                    out.push_str(&format!("|{}\n", "---|".repeat(headers.len())));
                    for row in rows {
                        let cells: Vec<String> =
                            row.iter().map(|cell| cell.replace('|', "\\|")).collect();
                        out.push_str(&format!("| {} |\n", cells.join(" | ")));
                    }
                    out.push('\n');
                }
                Block::Heading(heading) => out.push_str(&format!("### {heading}\n\n")),
                Block::Items(items) => {
                    for item in items {
                        out.push_str(&format!("- {item}\n"));
                    }
                    out.push('\n');
                }
                Block::Text(line) => out.push_str(&format!("_{line}_\n\n")),
            }
        }
    }
    out
}

/// The report as one self-contained HTML page, with a light and a dark theme.
pub(super) fn html(report: &Json) -> String {
    let (a, b) = (&report["runA"], &report["runB"]);
    let short = |run: &Json| text(&run["sessionId"]).chars().take(8).collect::<String>();
    let title = format!(
        "Run comparison · {} ({}) vs {} ({})",
        text(&a["runName"]),
        short(a),
        text(&b["runName"]),
        short(b)
    );
    let mut body = String::new();
    let mut links = String::new();
    for section in sections(report) {
        links.push_str(&format!(
            "<a href=\"#{}\">{}</a>",
            section.id,
            escape(section.title)
        ));
        body.push_str(&format!(
            "<section id=\"{}\"><h2>{}</h2>",
            section.id,
            escape(section.title)
        ));
        if let Some(note) = &section.note {
            body.push_str(&format!("<p class=\"note\">{}</p>", escape(note)));
        }
        for block in &section.blocks {
            match block {
                Block::Table(headers, rows) => {
                    body.push_str("<table><tr>");
                    for header in headers {
                        body.push_str(&format!("<th>{}</th>", escape(header)));
                    }
                    body.push_str("</tr>");
                    for row in rows {
                        body.push_str("<tr>");
                        for cell in row {
                            body.push_str(&format!("<td>{}</td>", escape(cell)));
                        }
                        body.push_str("</tr>");
                    }
                    body.push_str("</table>");
                }
                Block::Heading(heading) => body.push_str(&format!("<h3>{}</h3>", escape(heading))),
                Block::Items(items) => {
                    body.push_str("<ul>");
                    for item in items {
                        body.push_str(&format!("<li>{}</li>", escape(item)));
                    }
                    body.push_str("</ul>");
                }
                Block::Text(line) => body.push_str(&format!("<p><em>{}</em></p>", escape(line))),
            }
        }
        body.push_str("</section>");
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title><style>{STYLE}</style></head><body><nav>{links}</nav><main>\
         <h1>Run comparison</h1><p class=\"note\">Generated {}</p>{body}</main></body></html>\n",
        escape(&title),
        escape(&text(&report["generatedAt"])),
    )
}

const STYLE: &str = "\
:root{--text:#1d2329;--muted:#5b6670;--line:#d5dbe0;--back:#ffffff;--head:#f2f4f6}\
@media (prefers-color-scheme:dark){:root{--text:#e3e7ea;--muted:#9aa5ae;--line:#3a434b;\
--back:#161b20;--head:#20272e}}\
body{margin:0;font:15px/1.5 system-ui,sans-serif;color:var(--text);background:var(--back)}\
nav{position:sticky;top:0;display:flex;flex-wrap:wrap;gap:12px;padding:8px 16px;\
background:var(--head);border-bottom:1px solid var(--line)}\
nav a{color:var(--text)}main{max-width:1100px;margin:0 auto;padding:16px}\
table{border-collapse:collapse;margin:8px 0 16px;display:block;overflow-x:auto}\
th,td{border:1px solid var(--line);padding:4px 8px;text-align:left;vertical-align:top}\
th{background:var(--head)}.note{color:var(--muted)}";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A value as a cell shows it: text without quotes, `-` for none.
fn text(value: &Json) -> String {
    match value {
        Json::Null => "-".into(),
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn percent(value: &Json) -> String {
    value
        .as_f64()
        .map_or("-".into(), |ratio| format!("{:.0}%", ratio * 100.0))
}

fn rows(value: &Json) -> &[Json] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The sections of the pages, from the JSON report.
fn sections(report: &Json) -> Vec<Section> {
    let summary = &report["summary"];
    let (a, b) = (&report["runA"], &report["runB"]);
    let count = |key: &str| text(&summary[key]);
    let mut sections = vec![Section {
        id: "summary",
        title: "Summary",
        note: None,
        blocks: vec![Block::Table(
            vec![
                "Changed",
                "Only in B",
                "Only in A",
                "Unchanged",
                "Recomputed",
                "Software",
                "Regressions",
            ],
            vec![
                [
                    "tasksChanged",
                    "tasksAdded",
                    "tasksRemoved",
                    "tasksUnchanged",
                    "tasksRecomputed",
                    "softwareChanged",
                    "regressions",
                ]
                .map(count)
                .to_vec(),
            ],
        )],
    }];
    let runs = [
        ("Run name", "runName"),
        ("Session id", "sessionId"),
        ("Status", "status"),
        ("Revision", "revision"),
        ("Command", "command"),
        ("Launched", "launched"),
        ("Wall time (ms)", "wallDurationMillis"),
        ("Tasks", "taskCount"),
        ("Cached", "cachedTasks"),
    ];
    let runs = runs
        .iter()
        .map(|(label, key)| vec![label.to_string(), text(&a[key]), text(&b[key])]);
    sections.push(Section {
        id: "runs",
        title: "Runs",
        note: None,
        blocks: vec![Block::Table(vec!["", "Run A", "Run B"], runs.collect())],
    });
    let changed = |key: &str| -> Vec<&Json> {
        rows(&report[key])
            .iter()
            .filter(|row| row["changed"] == true && row["obvious"] == false)
            .collect()
    };
    let fields = |key: &str, empty: &str, source: bool| {
        let found = changed(key);
        if found.is_empty() {
            return vec![Block::Text(empty.into())];
        }
        let rows = found.iter().map(|row| {
            let mut cells = vec![
                text(&row["field"]),
                text(&row["valueA"]),
                text(&row["valueB"]),
            ];
            if source {
                cells.push(format!(
                    "A:{} B:{}",
                    text(&row["sourceA"]),
                    text(&row["sourceB"])
                ));
            }
            cells
        });
        let mut headers = vec!["Field", "Run A", "Run B"];
        if source {
            headers.push("Source");
        }
        vec![Block::Table(headers, rows.collect())]
    };
    sections.push(Section {
        id: "metadata",
        title: "Run metadata",
        note: None,
        blocks: fields("metadata", "No metadata changes.", false),
    });
    sections.push(Section {
        id: "params",
        title: "Parameters & options",
        note: None,
        blocks: fields("params", "No parameter changes.", true),
    });
    sections.push(Section {
        id: "config",
        title: "Resolved configuration",
        note: Some(text(&report["configNote"])),
        blocks: Vec::new(),
    });
    let processes = rows(&report["processes"]).iter().map(|row| {
        ["process", "countA", "countB", "status"]
            .map(|key| text(&row[key]))
            .to_vec()
    });
    sections.push(Section {
        id: "processes",
        title: "Process topology",
        note: None,
        blocks: vec![Block::Table(
            vec!["Process", "Run A", "Run B", "Status"],
            processes.collect(),
        )],
    });
    let software: Vec<Vec<String>> = rows(&report["software"])
        .iter()
        .filter(|row| row["status"] == "changed")
        .map(|row| {
            let join = |key: &str| {
                rows(&row[key])
                    .iter()
                    .map(text)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            vec![
                text(&row["process"]),
                join("containersA"),
                join("containersB"),
            ]
        })
        .collect();
    let software = match software.is_empty() {
        true => vec![Block::Text(
            "No container changes: every process ran with the same images.".into(),
        )],
        false => vec![Block::Table(
            vec!["Process", "Containers A", "Containers B"],
            software,
        )],
    };
    sections.push(Section {
        id: "software",
        title: "Software & versions",
        note: None,
        blocks: software,
    });
    let threshold = text(&report["perfThreshold"]);
    let regressions: Vec<Vec<String>> = rows(&report["regressions"])
        .iter()
        .map(|row| {
            let mut cells = ["key", "metric", "valueA", "valueB"]
                .map(|key| text(&row[key]))
                .to_vec();
            cells.push(format!("{:+}%", row["deltaPct"].as_f64().unwrap_or(0.0)));
            cells.push(if row["sameWork"] == true { "yes" } else { "no" }.into());
            cells
        })
        .collect();
    let regressions = match regressions.is_empty() {
        true => vec![Block::Text(format!(
            "No task metric changed by ≥ {threshold}% between the runs."
        ))],
        false => vec![Block::Table(
            vec!["Task", "Metric", "Run A", "Run B", "Change", "Same work"],
            regressions,
        )],
    };
    sections.push(Section {
        id: "regressions",
        title: "Performance regressions",
        note: None,
        blocks: regressions,
    });
    let efficiency = rows(&report["efficiency"]).iter().map(|row| {
        let class = |key: &str| text(&row[key]);
        vec![
            text(&row["process"]),
            percent(&row["cpuEfficiencyA"]),
            percent(&row["cpuEfficiencyB"]),
            class("cpuClassB"),
            percent(&row["memEfficiencyA"]),
            percent(&row["memEfficiencyB"]),
            class("memClassB"),
        ]
    });
    sections.push(Section {
        id: "efficiency",
        title: "Resource efficiency",
        note: Some("Peak usage against what each process asked for: over-provisioned below 50 %, tight at 90 % or more.".into()),
        blocks: vec![Block::Table(vec!["Process", "CPU eff. A", "CPU eff. B", "CPU class", "Mem eff. A", "Mem eff. B", "Mem class"], efficiency.collect())],
    });
    let mut tasks = Vec::new();
    for task in rows(&report["tasks"]) {
        tasks.push(Block::Heading(format!(
            "{} ({})",
            text(&task["key"]),
            text(&task["kind"])
        )));
        let mut items: Vec<String> = rows(&task["fields"])
            .iter()
            .map(|row| {
                format!(
                    "{}: `{}` → `{}`",
                    text(&row["field"]),
                    text(&row["valueA"]),
                    text(&row["valueB"])
                )
            })
            .collect();
        items.extend(rows(&task["notes"]).iter().map(text));
        if !items.is_empty() {
            tasks.push(Block::Items(items));
        }
    }
    if tasks.is_empty() {
        tasks.push(Block::Text("No task changes.".into()));
    }
    sections.push(Section {
        id: "tasks",
        title: "Task detail",
        note: None,
        blocks: tasks,
    });
    let mut outputs = Vec::new();
    for task in rows(&report["outputs"]) {
        outputs.push(Block::Heading(text(&task["key"])));
        let files = rows(&task["files"]).iter().map(|file| {
            ["file", "sizeA", "sizeB", "status"]
                .map(|key| text(&file[key]))
                .to_vec()
        });
        outputs.push(Block::Table(
            vec!["File", "Bytes A", "Bytes B", "Status"],
            files.collect(),
        ));
    }
    if outputs.is_empty() {
        outputs.push(Block::Text("No output file differences.".into()));
    }
    sections.push(Section {
        id: "outputs",
        title: "Output files",
        note: Some(text(&report["outputsNote"])),
        blocks: outputs,
    });
    sections
}

#[cfg(test)]
mod tests {
    use run_database::{Attempt, FileRow, Side, TaskRow};

    use super::*;

    fn run(id: &str) -> Run {
        Run {
            id: id.into(),
            lineage: id.into(),
            resumes: None,
            workflow: "/w/main.nf".into(),
            language: "nextflow".into(),
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: None,
            process: 1,
            started: 0,
            ended: Some(1000),
            state: "succeeded".into(),
            error: None,
            replays: None,
            replays_task: None,
            tasks: BTreeMap::new(),
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        }
    }

    /// A task that ran 10 s with this CPU time, memory peak, exit code and output digest.
    fn detail(cpu_seconds: i64, peak: i64, exit: i32, digest: &str) -> TaskDetail {
        let mut description = workflow_graph::Task::default();
        description.resources.cpus = Some(2.0);
        description.resources.memory_bytes = Some(1000);
        TaskDetail {
            row: TaskRow {
                id: 1,
                step: "ALIGN".into(),
                name: "ALIGN (a)".into(),
                state: "succeeded".into(),
                requested: 0,
                attempts: 1,
                started: Some(0),
                ended: Some(10_000),
                exit_code: Some(exit),
                reused: None,
            },
            description,
            fingerprint: digest.into(),
            attempts: vec![Attempt {
                number: 1,
                started: 0,
                ended: Some(10_000),
                exit_code: Some(exit),
                accepted: None,
                reused: None,
                image: None,
                retried: false,
                cpu_usec: Some(cpu_seconds * 1_000_000),
                memory_peak: Some(peak),
                read_bytes: None,
                write_bytes: None,
            }],
            inputs: Vec::new(),
            outputs: vec![FileRow {
                task: 1,
                attempt: Some(1),
                role: "output".into(),
                path: "out.bam".into(),
                location: "store".into(),
                directory: false,
                digest: Some(digest.into()),
                size: Some(10),
            }],
            checks: Vec::new(),
        }
    }

    #[test]
    fn compares_usage_layers() {
        let side = |task: u64, exit: i32| Side {
            task,
            name: "ALIGN (a)".into(),
            state: if exit == 0 { "succeeded" } else { "failed" }.into(),
            exit_code: Some(exit),
            reused: None,
            image: Some(format!("aligner:{task}")),
        };
        let comparison = Comparison {
            first: run("a"),
            second: run("b"),
            changes: Vec::new(),
            tasks: vec![TaskPair {
                step: "ALIGN".into(),
                pairing: Pairing::Outputs,
                first: Some(side(1, 0)),
                second: Some(side(2, 1)),
                changes: Vec::new(),
                outputs: Vec::new(),
                results: Vec::new(),
            }],
        };
        let details = Details {
            first: HashMap::from([(1, detail(19, 500, 0, "x"))]),
            second: HashMap::from([(2, detail(4, 950, 1, "y"))]),
        };
        let report = report(&comparison, &details, 0);
        assert_eq!(report["identical"], false);
        let summary = &report["summary"];
        assert_eq!(summary["softwareChanged"], 1);
        assert_eq!(summary["outputsChanged"], 1);
        assert_eq!(
            (summary["failedA"].clone(), summary["newFailures"].clone()),
            (json!(0), json!(1))
        );
        // Memory grew from 500 to 950 bytes (+90 %); the CPU time is no metric of nf-diff.
        let regression = &report["regressions"][0];
        assert_eq!(
            (regression["metric"].clone(), regression["deltaPct"].clone()),
            (json!("peak_rss"), json!(90.0))
        );
        assert_eq!(summary["regressions"], 1);
        let efficiency = &report["efficiency"][0];
        assert_eq!(efficiency["cpuClassA"], "tight");
        assert_eq!(efficiency["cpuClassB"], "over-provisioned");
        assert_eq!(efficiency["memClassB"], "tight");
        assert_eq!(efficiency["memClassA"], "right-sized");
        assert_eq!(report["outputs"][0]["files"][0]["status"], "changed");
        let fields = &report["tasks"][0]["fields"];
        let names: Vec<&str> = fields
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["field"].as_str())
            .collect();
        assert_eq!(names, ["status", "exit", "container"]);
    }

    #[test]
    fn renders_report_pages() {
        let mut comparison = Comparison {
            first: run("a"),
            second: run("b"),
            changes: Vec::new(),
            tasks: Vec::new(),
        };
        comparison.second.parameters = vec![("greeting".into(), "<hi>".into())];
        let report = report(&comparison, &Details::default(), 0);
        let markdown = markdown(&report);
        assert!(
            markdown.starts_with("# Run comparison report\n\n> Generated 1970-01-01T00:00:00.000Z")
        );
        assert!(markdown.contains("## Parameters & options\n\n| Field | Run A | Run B | Source |\n|---|---|---|---|\n| --greeting | - | <hi> | A:- B:CLI |\n"), "{markdown}");
        assert!(markdown.contains("## Task detail\n\n_No task changes._"));
        let page = html(&report);
        assert!(page.contains("<title>Run comparison · a (a) vs b (b)</title>"));
        assert!(page.contains("<section id=\"tasks\">") && page.contains("&lt;hi&gt;"));
        assert!(!page.contains("<hi>"));
    }
}
