//! Compiles Snakemake workflows into wir: the jobs that the planner of uwe plans become dialect
//! tasks, and a workflow with parts that have no form in wir yet one native Snakemake op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod jobs;
mod launch;
mod region;

use std::path::Path;

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use snakemake_runtime::{PlannedJob, RunSettings, Settings};
use wir::Document;
use workflow_graph::Launch;

pub use launch::{
    group_keys, job_key, job_launch, place_values, plan_launch, plans_job, rule_values,
    source_values,
};
pub use region::region_plan;

/// The id of the source of the Snakefile in a compiled document.
pub const SOURCE: &str = "snakefile";

/// A compiled workflow, and the parts of it that stay a native Snakemake op.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub document: Document,
    pub kept: Vec<Kept>,
}

/// An op that stays a native Snakemake op, and the first reason why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

impl Compiled {
    /// Whether no part of the workflow stays native.
    pub fn full(&self) -> bool {
        self.kept.is_empty()
    }
}

/// Compiles the workflow of `launch` with the options `run` as uwe plans it, with source
/// locations relative to `base`; it fails when planning fails, as a run would.
pub fn compile(
    launch: &Launch,
    settings: Settings,
    run: RunSettings,
    base: &Path,
) -> Result<Compiled, String> {
    let sources = sources(launch, base);
    let configs: Vec<String> = (sources.iter().skip(1))
        .filter_map(|source| source["id"].as_str().map(str::to_owned))
        .collect();
    let mut sources = sources;
    let values = (!launch.params.is_empty()).then(|| {
        sources.push(launch::values(&launch.params));
        launch::VALUES.to_owned()
    });
    let setting = Setting {
        entry: launch.targets.first().cloned(),
        profiles: launch.profiles.clone(),
        configs,
        values,
    };
    let folder = run.workdir.clone();
    let place = |sources: Vec<Json>| started(sources, launch, folder.as_deref(), base);
    // The code of the Snakefile writes its files when the run runs it, not here.
    let run = RunSettings {
        planning: true,
        ..run
    };
    let mut driver =
        snakemake_runtime::spawn_driver(launch, settings, run).map_err(|error| error.message)?;
    let jobs = match driver.plan_jobs() {
        Ok(jobs) => jobs,
        // A native op holds what uwe cannot plan job by job; its run says the same.
        Err(error) if error.unsupported => {
            let reason = error.message.lines().next().unwrap_or_default().to_owned();
            return native(place(sources)?, &setting, launch, reason);
        }
        Err(error) => return Err(error.message),
    };
    let targets = driver.plan_targets().map_err(|error| error.message)?;
    let first = driver.default_target().map_err(|error| error.message)?;
    drop(driver);
    if let Some(reason) = jobs.iter().find_map(kept_job) {
        return native(place(sources)?, &setting, launch, reason);
    }
    // Without targets the run plans the first rule, also when options like `-until` or
    // `-forcerun` add or remove roots of the plan.
    let root = first.or_else(|| jobs.iter().find_map(|job| job.target.clone()));
    let setting = Setting {
        entry: setting.entry.clone().or(root),
        ..setting
    };
    let checkpoint = jobs.iter().find(|job| job.checkpoint);
    let graph = match checkpoint {
        Some(_) => region(&setting, launch)?,
        None => jobs::graph(&jobs, &setting, launch, &targets),
    };
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": sources,
        "graphs": [graph],
        "entry": "main",
    });
    let document = serde_json::from_value(document).map_err(|error| error.to_string())?;
    Ok(Compiled {
        document,
        kept: Vec::new(),
    })
}

/// What every op of a compiled workflow runs with: the target of the plan, the profiles, the
/// ids of the config sources and of the source of the config values.
pub(crate) struct Setting {
    pub entry: Option<String>,
    pub profiles: Vec<String>,
    pub configs: Vec<String>,
    pub values: Option<String>,
}

/// Sets the options of `launch` that pick the jobs of a plan on a planner or a native op.
fn pick_jobs(target: &mut Json, launch: &Launch) {
    let lists = [
        ("until", &launch.until),
        ("omit_from", &launch.omit_from),
        ("forcerun", &launch.forcerun),
    ];
    for (field, list) in lists.into_iter().filter(|(_, list)| !list.is_empty()) {
        target[field] = json!(list);
    }
    if launch.forceall {
        target["forceall"] = json!(true);
    }
}

/// Why a job keeps the workflow native, like a group of several images; a job that waits for
/// a checkpoint runs in a region.
fn kept_job(job: &PlannedJob) -> Option<String> {
    let rule = &job.key.rule;
    let why = job
        .native
        .as_ref()
        .filter(|why| *why != snakemake_runtime::WAITING)?;
    Some(format!("a job of rule {rule}: {why}"))
}

/// The entry graph of a workflow with checkpoints: one dynamic region that the Snakemake
/// planner plans again after each checkpoint, as a native run does.
fn region(setting: &Setting, launch: &Launch) -> Result<Json, String> {
    let targets = match launch.targets.is_empty() {
        true => setting.entry.iter().cloned().collect(),
        false => launch.targets.clone(),
    };
    let mut planner = json!({"source": SOURCE, "targets": targets});
    if !setting.profiles.is_empty() {
        planner["profiles"] = json!(setting.profiles);
    }
    if !setting.configs.is_empty() {
        planner["configs"] = json!(setting.configs);
    }
    // The region plans with the config values and the options that pick jobs of the run.
    if let Some(values) = &setting.values {
        planner["config_values"] = json!(values);
    }
    pick_jobs(&mut planner, launch);
    let kind = json!({"dynamic": {"planner": {"snakemake": planner}}});
    Ok(json!({"name": "main", "ops": [{"path": "snakemake", "kind": kind}]}))
}

/// The whole workflow as one native Snakemake op, which plans its jobs with the config values
/// and the options of the launch, as the run would.
fn native(
    sources: Vec<Json>,
    setting: &Setting,
    launch: &Launch,
    reason: String,
) -> Result<Compiled, String> {
    let path = "snakemake";
    let mut native = json!({"source": SOURCE, "language": "snakemake"});
    if let Some(entry) = &setting.entry {
        native["entry"] = json!(entry);
    }
    if !setting.profiles.is_empty() {
        native["profiles"] = json!(setting.profiles);
    }
    if !setting.configs.is_empty() {
        native["configs"] = json!(setting.configs);
    }
    if launch.targets.len() > 1 {
        native["targets"] = json!(launch.targets);
    }
    if let Some(values) = &setting.values {
        native["config_values"] = json!(values);
    }
    pick_jobs(&mut native, launch);
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": sources,
        "graphs": [{"name": "main", "ops": [{"path": path, "kind": {"native": native}}]}],
        "entry": "main",
    });
    Ok(Compiled {
        document: serde_json::from_value(document).expect("a native document"),
        kept: vec![Kept {
            path: path.to_owned(),
            reason,
        }],
    })
}

/// The sources of a native op, which starts in the folder of its Snakefile source: for a
/// Snakefile, the working folder of the launch, which must find that Snakefile itself.
fn started(
    mut sources: Vec<Json>,
    launch: &Launch,
    folder: Option<&Path>,
    base: &Path,
) -> Result<Vec<Json>, String> {
    if launch.target.is_dir() {
        return Ok(sources);
    }
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let folder = folder.map_or(current, Path::to_owned);
    let same = |path: &Path| std::fs::canonicalize(path).ok();
    let found = snakemake_runtime::find_snakefile(&folder);
    if found
        .as_deref()
        .and_then(same)
        .is_none_or(|found| Some(found) != same(&launch.target))
    {
        return Err(format!(
            "the workflow stays one native Snakemake op, which starts in {} but finds no {} there",
            folder.display(),
            launch.target.display()
        ));
    }
    sources[0] = source(SOURCE, &folder, found.as_deref(), base);
    Ok(sources)
}

/// The source of the Snakefile, then the config files of the launch, each with the digest of
/// its bytes; a project folder is pinned by the digest of its Snakefile.
fn sources(launch: &Launch, base: &Path) -> Vec<Json> {
    let file = match launch.target.is_dir() {
        true => snakemake_runtime::find_snakefile(&launch.target),
        false => Some(launch.target.clone()),
    };
    let mut sources = vec![source(SOURCE, &launch.target, file.as_deref(), base)];
    let configs = launch.configs.iter().chain(&launch.inputs);
    for (index, config) in configs.enumerate() {
        let id = format!("config.{}", index + 1);
        sources.push(source(&id, config, Some(config), base));
    }
    sources
}

fn source(id: &str, path: &Path, file: Option<&Path>, base: &Path) -> Json {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let location = match absolute.strip_prefix(base) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => absolute.to_string_lossy().into_owned(),
    };
    let mut source = json!({"id": id, "language": "snakemake", "location": location});
    if let Some(bytes) = file.and_then(|file| std::fs::read(file).ok()) {
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        source["digest"] = json!(format!("sha256:{hex}"));
    }
    source
}
