//! Compiles Snakemake workflows into wir: the jobs that the planner of uwe plans become dialect
//! tasks, and a workflow with parts that have no form in wir yet one native Snakemake op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod jobs;

use std::path::Path;

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use snakemake_runtime::{PlannedJob, RunSettings, Settings};
use wir::Document;
use workflow_graph::Launch;

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
    let setting = Setting {
        entry: launch.targets.first().cloned(),
        profiles: launch.profiles.clone(),
        configs,
    };
    if let Some(reason) = unheld(launch) {
        return Ok(native(sources, &setting, reason));
    }
    let mut driver =
        snakemake_runtime::spawn_driver(launch, settings, run).map_err(|error| error.message)?;
    let jobs = match driver.plan_jobs() {
        Ok(jobs) => jobs,
        // A native op holds what uwe cannot plan job by job; its run says the same.
        Err(error) if error.unsupported => {
            let reason = error.message.lines().next().unwrap_or_default().to_owned();
            return Ok(native(sources, &setting, reason));
        }
        Err(error) => return Err(error.message),
    };
    drop(driver);
    if let Some(reason) = jobs.iter().find_map(kept_job) {
        return Ok(native(sources, &setting, reason));
    }
    let setting = Setting {
        entry: (setting.entry.clone())
            .or_else(|| (jobs.iter().find(|job| job.target)).map(|job| job.key.rule.clone())),
        ..setting
    };
    let graph = jobs::graph(&jobs, &setting);
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

/// What every op of a compiled workflow runs with: the target of the plan, the profiles and
/// the ids of the config sources.
pub(crate) struct Setting {
    pub entry: Option<String>,
    pub profiles: Vec<String>,
    pub configs: Vec<String>,
}

/// Why the settings of a launch keep the workflow native: a document holds neither config
/// values of the command line nor the options that pick the jobs of a plan.
fn unheld(launch: &Launch) -> Option<String> {
    let held = [
        (
            !launch.params.is_empty(),
            "config values of the command line",
        ),
        (launch.targets.len() > 1, "several targets"),
        (!launch.until.is_empty(), "the option until"),
        (!launch.omit_from.is_empty(), "the option omit-from"),
        (!launch.forcerun.is_empty(), "the option forcerun"),
        (launch.forceall, "the option forceall"),
    ];
    let (_, reason) = held.into_iter().find(|(set, _)| *set)?;
    Some(format!("the run sets {reason}"))
}

/// Why a job keeps the workflow native: checkpoints need a dynamic region, and jobs that run
/// together in one task have no form in wir yet.
fn kept_job(job: &PlannedJob) -> Option<String> {
    let rule = &job.key.rule;
    if job.checkpoint {
        return Some(format!("rule {rule} is a checkpoint"));
    }
    let why = job.native.as_ref()?;
    Some(format!("a job of rule {rule}: {why}"))
}

/// The whole workflow as one native Snakemake op.
fn native(sources: Vec<Json>, setting: &Setting, reason: String) -> Compiled {
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
    let document = json!({
        "wir_version": wir::VERSION,
        "sources": sources,
        "graphs": [{"name": "main", "ops": [{"path": path, "kind": {"native": native}}]}],
        "entry": "main",
    });
    Compiled {
        document: serde_json::from_value(document).expect("a native document"),
        kept: vec![Kept {
            path: path.to_owned(),
            reason,
        }],
    }
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
