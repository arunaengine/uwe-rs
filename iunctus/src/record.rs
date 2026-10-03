//! Every run records itself in the run database (decision 10), simulated or real. This module
//! starts the record of a run, and finds the earlier run that `-resume` continues.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use run_database::{Database, Recorder, Reusable, Run, RunStart};
use workflow_graph::Language;

use crate::arguments::{Arguments, Resume};
use crate::sources::Sources;
use crate::{Failure, failure, revision, runs};

/// A run being recorded, and the finished tasks of the runs it continues.
pub struct Recording {
    pub recorder: Recorder,
    /// The finished tasks of the lineage by fingerprint, when the run continues another.
    pub reusable: Option<HashMap<String, Reusable>>,
}

/// What the run is, besides the command line.
pub struct Started<'a> {
    pub id: &'a str,
    pub target: &'a Path,
    pub language: Language,
    pub executor: &'a str,
    pub simulated: bool,
    pub store: Option<&'a str>,
    /// The files of the workflow and of the command line, by digest.
    pub sources: &'a Sources,
    /// The run that this one repeats.
    pub repeats: Option<&'a str>,
    /// Explicit invocation for an embedded run; CLI runs retain their original arguments.
    pub invocation: Option<&'a [String]>,
}

/// Opens the run database and starts the record of a run. With `-resume`, the run joins the
/// lineage of the run it continues, and gets the finished tasks of that lineage.
pub fn start(arguments: &Arguments, run: &Started) -> Result<Recording, Failure> {
    let broken = |error: run_database::DatabaseError| Failure::setup(format!("iunctus: {error}"));
    let database = runs::open(arguments)?;
    let workflow = std::path::absolute(run.target)
        .unwrap_or_else(|_| run.target.to_owned())
        .to_string_lossy()
        .into_owned();
    let resumed = match &arguments.resume {
        Some(resume) => resumed(&database, resume, &workflow, run.simulated)?,
        None => None,
    };
    let reusable = match &resumed {
        Some(earlier) => {
            let lineage = database.lineage(&earlier.lineage).map_err(broken)?;
            Some(database.reusable(&lineage).map_err(broken)?)
        }
        None => None,
    };
    let start = RunStart {
        id: run.id.to_owned(),
        resumes: resumed.map(|earlier| earlier.id),
        workflow,
        language: run.language,
        revision: revision::revision(run.target),
        arguments: if let Some(invocation) = run.invocation {
            invocation.to_vec()
        } else if run.repeats.is_some() {
            run_arguments(arguments)
        } else {
            std::env::args().skip(1).collect()
        },
        parameters: arguments.params.clone(),
        executor: run.executor.to_owned(),
        simulated: run.simulated,
        store: run.store.map(str::to_owned),
        program: Some(format!("iunctus {}", env!("CARGO_PKG_VERSION"))),
        launch: (std::env::current_dir().ok()).map(|folder| folder.to_string_lossy().into_owned()),
        workflow_folder: Some(run.sources.folder.to_string_lossy().into_owned()),
        repeats: run.repeats.map(str::to_owned),
    };
    let mut recorder = database.record(&start).map_err(broken)?;
    recorder.sources(&run.sources.files);
    Ok(Recording { recorder, reusable })
}

pub(crate) fn run_arguments(arguments: &Arguments) -> Vec<String> {
    let mut words = vec!["run".into()];
    let path = |path: &std::path::PathBuf| {
        std::path::absolute(path)
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .into_owned()
    };
    if let Some(target) = &arguments.target {
        words.push(path(target));
    }
    macro_rules! text {
        ($($field:ident => $option:literal),* $(,)?) => {
            $(if let Some(value) = &arguments.$field {
                words.extend([$option.into(), value.to_string()]);
            })*
        };
    }
    macro_rules! paths {
        ($($field:ident => $option:literal),* $(,)?) => {
            $(if let Some(value) = &arguments.$field {
                words.extend([$option.into(), path(value)]);
            })*
        };
    }
    macro_rules! flags {
        ($($field:ident => $option:literal),* $(,)?) => {
            $(if arguments.$field { words.push($option.into()); })*
        };
    }
    text!(language => "-lang", store => "-store", executor => "-executor",
        tes_server => "-tes-server", slurm_container => "-slurm-container",
        slurm_options => "-slurm-options", kubernetes_namespace => "-kubernetes-namespace",
        default_image => "-default-image", python_image => "-python-image",
        max_pending => "-max-pending", address => "-address", port => "-port",
        format => "-format", fold => "-fold", platform => "-platform", license => "-license",
        keep_days => "-keep-days");
    paths!(config_file => "-iunctus-config", inputs => "-inputs", graph => "-graph",
        report => "-report", output_dir => "-output-dir", kubeconfig => "-kubeconfig",
        database => "-database", output => "-output");
    flags!(stub => "-stub", keep_going => "-keep-going", page => "-page",
        no_cache => "-no-cache", no_network => "-no-network", no_right_sizing => "-no-right-sizing",
        no_grouping => "-no-grouping", check => "-check", remove => "-remove");
    if let Some(profiles) = &arguments.profiles {
        words.extend(["-profile".into(), profiles.join(",")]);
    }
    for config in &arguments.configs {
        words.extend(["-configfile".into(), path(config)]);
    }
    for (name, value) in &arguments.params {
        words.push(format!("--{name}={value}"));
    }
    for target in &arguments.targets {
        words.extend(["-entry".into(), target.clone()]);
    }
    if let Some(order) = arguments.order {
        let order = match order {
            workflow_graph::Order::AsAsked => "asked".into(),
            workflow_graph::Order::Reversed => "reversed".into(),
            workflow_graph::Order::Finished => "finished".into(),
            workflow_graph::Order::Shuffled { seed } => format!("shuffled:{seed}"),
        };
        words.extend(["-order".into(), order]);
    }
    if let Some(view) = arguments.view {
        let view = match view {
            workflow_graph::render::View::Tasks => "tasks",
            workflow_graph::render::View::Steps => "steps",
        };
        words.extend(["-view".into(), view.into()]);
    }
    if let Some(direction) = arguments.direction {
        words.extend(["-direction".into(), direction.name().into()]);
    }
    if let Some(share) = arguments.check_reproducibility {
        words.extend(["-check-reproducibility".into(), (share * 100.0).to_string()]);
    }
    words
}

/// The run that `-resume` names, checked: it ran the same workflow, it has ended, and a real
/// run continues only a real run, since the files of a simulated one are made up. Without an
/// earlier run of the workflow, the run starts from the beginning.
fn resumed(
    database: &Database,
    resume: &Resume,
    workflow: &str,
    simulated: bool,
) -> Result<Option<Run>, Failure> {
    let broken = |error: run_database::DatabaseError| failure(format!("iunctus: {error}"));
    let earlier = match resume {
        Resume::Last => database.last_run(workflow, !simulated).map_err(broken)?,
        Resume::Run(id) => match database.run(id).map_err(broken)? {
            Some(run) => Some(run),
            None => {
                return Err(failure(format!(
                    "iunctus: the run database has no run `{id}` to resume; `iunctus runs` \
                     lists the runs"
                )));
            }
        },
    };
    let Some(earlier) = earlier else {
        eprintln!(
            "iunctus: no earlier run of {workflow} to resume; the run starts from the beginning"
        );
        return Ok(None);
    };
    if earlier.workflow != workflow {
        return Err(failure(format!(
            "iunctus: run {} ran {}, not {workflow}; resume a run of the same workflow",
            earlier.id, earlier.workflow
        )));
    }
    if earlier.state == "running" {
        return Err(failure(format!(
            "iunctus: run {} is still running; resume it when it has stopped",
            earlier.id
        )));
    }
    if earlier.simulated && !simulated {
        return Err(failure(format!(
            "iunctus: run {} was simulated, so its files are made up; a real run cannot \
             resume it",
            earlier.id
        )));
    }
    Ok(Some(earlier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_repeat_options_roundtrip() {
        let arguments = Arguments {
            target: Some("/restored/workflow/main.nf".into()),
            language: Some(Language::Nextflow),
            profiles: Some(vec!["analysis".into(), "local".into()]),
            configs: vec!["/restored/named/settings,extra.config".into()],
            inputs: Some("/restored/named/params.json".into()),
            params: vec![
                ("reads".into(), "a b.fq".into()),
                ("flag".into(), "false".into()),
            ],
            targets: vec!["CUSTOM".into()],
            executor: Some("local".into()),
            store: Some("/cache/store".into()),
            default_image: Some("busybox:stable".into()),
            python_image: Some("python:3".into()),
            stub: true,
            keep_going: true,
            no_cache: true,
            no_network: true,
            no_right_sizing: true,
            no_grouping: true,
            check_reproducibility: Some(0.25),
            max_pending: Some(3),
            order: Some(workflow_graph::Order::Shuffled { seed: 42 }),
            output_dir: Some("/results".into()),
            ..Arguments::default()
        };
        let words = run_arguments(&arguments);
        assert_eq!(
            crate::arguments::parse(&words[1..]).expect("parses"),
            arguments
        );
    }
}
