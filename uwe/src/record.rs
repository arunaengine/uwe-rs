//! Every run records itself in the run database (decision 10), simulated or real. This module
//! starts the record of a run, and finds the earlier run that `-resume` continues.
// Copyright (c) 2026 The uwe Contributors
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

/// Strict runs publish their terminal state only after provenance retention succeeds.
pub(crate) struct Terminal<'a> {
    pub observer: &'a mut dyn workflow_graph::Observer,
    pub defer: bool,
}

impl workflow_graph::Observer for Terminal<'_> {
    fn event(&mut self, event: workflow_graph::Event<'_>) {
        if !self.defer || !matches!(event, workflow_graph::Event::Ended { .. }) {
            self.observer.event(event);
        }
    }
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
    pub strict_provenance: bool,
    /// The wir document that the run compiled its workflow into.
    pub document: Option<&'a Path>,
}

/// Opens the run database and starts the record of a run. With `-resume`, the run joins the
/// lineage of the run it continues, and gets the finished tasks of that lineage.
pub fn start(arguments: &Arguments, run: &Started) -> Result<Recording, Failure> {
    let broken = |error: run_database::DatabaseError| Failure::setup(format!("uwe: {error}"));
    let database = runs::open(arguments)?;
    let database = if run.strict_provenance {
        database.durable().map_err(broken)?
    } else {
        database
    };
    let workflow = if storage::address::scheme(&run.target.to_string_lossy()).is_some() {
        run.target.to_owned()
    } else {
        std::path::absolute(run.target).unwrap_or_else(|_| run.target.to_owned())
    }
    .to_string_lossy()
    .into_owned();
    let resumed = match &arguments.resume {
        Some(resume) => resumed(&database, resume, &workflow, run)?,
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
        program: Some(env!("UWE_IDENTITY").to_owned()),
        launch: (std::env::current_dir().ok()).map(|folder| folder.to_string_lossy().into_owned()),
        workflow_folder: Some(run.sources.folder.to_string_lossy().into_owned()),
        repeats: run.repeats.map(str::to_owned),
    };
    let mut recorder = database.record(&start).map_err(broken)?;
    recorder.sources(&run.sources.files);
    if let Some(document) = run.document {
        let read = std::fs::read(document);
        let read = read.map_err(|error| failure(format!("uwe: {}: {error}", document.display())));
        let digest = task_model::Digest::sha256(&read?);
        recorder.document(
            &document.to_string_lossy(),
            &format!("sha256:{}", digest.hex()),
        );
    }
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
        keep_days => "-keep-days", revision => "-r");
    paths!(config_file => "-uwe-config", inputs => "-inputs", graph => "-graph",
        report => "-report", output_dir => "-output-dir", kubeconfig => "-kubeconfig",
        database => "-database", output => "-output");
    flags!(stub => "-stub", keep_going => "-keep-going", page => "-page",
        no_cache => "-no-cache", no_network => "-no-network", no_right_sizing => "-no-right-sizing",
        no_grouping => "-no-grouping", check => "-check", remove => "-remove",
        latest => "-latest");
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
    run: &Started,
) -> Result<Option<Run>, Failure> {
    let simulated = run.simulated;
    let broken = |error: run_database::DatabaseError| failure(format!("uwe: {error}"));
    let earlier = match resume {
        Resume::Last => database.last_run(workflow, !simulated).map_err(broken)?,
        Resume::Run(id) => match database.run(id).map_err(broken)? {
            Some(run) => Some(run),
            None => {
                return Err(failure(format!(
                    "uwe: the run database has no run `{id}` to resume; `uwe runs` \
                     lists the runs"
                )));
            }
        },
    };
    let Some(earlier) = earlier else {
        eprintln!("uwe: no earlier run of {workflow} to resume; the run starts from the beginning");
        return Ok(None);
    };
    // A run of a compiled workflow may run another document than the run it continues, like
    // an earlier run that recorded its document as its workflow.
    let document = run.document.map(|path| path.to_string_lossy().into_owned());
    let compiled = match (earlier.document.as_deref(), document.as_deref()) {
        (Some(one), Some(other)) => crate::lower::same_workflow(one, other),
        _ => false,
    };
    if earlier.workflow != workflow && !compiled {
        return Err(failure(format!(
            "uwe: run {} ran {}, not {workflow}; resume a run of the same workflow",
            earlier.id, earlier.workflow
        )));
    }
    if earlier.state == "running" {
        return Err(failure(format!(
            "uwe: run {} is still running; resume it when it has stopped",
            earlier.id
        )));
    }
    if earlier.simulated && !simulated {
        return Err(failure(format!(
            "uwe: run {} was simulated, so its files are made up; a real run cannot \
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
    fn defers_terminal_state() {
        use workflow_graph::{Event, Observer};
        for error in [None, Some("retention failed")] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("runs.sqlite");
            let start = RunStart {
                id: "strict".into(),
                resumes: None,
                workflow: "/workflow/main.nf".into(),
                language: Language::Nextflow,
                revision: None,
                arguments: vec![],
                parameters: vec![],
                executor: "local".into(),
                simulated: false,
                store: None,
                program: None,
                launch: None,
                workflow_folder: None,
                repeats: None,
            };
            let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
            Terminal {
                observer: &mut recorder,
                defer: true,
            }
            .event(Event::Ended { error: None });
            let database = Database::open(&path).unwrap();
            assert_eq!(database.run("strict").unwrap().unwrap().state, "running");
            assert!(database.events("strict", 0, 100).unwrap().is_empty());
            recorder.ended(error);
            recorder.close().unwrap();
            let run = database.run("strict").unwrap().unwrap();
            assert_eq!(
                run.state,
                if error.is_some() {
                    "failed"
                } else {
                    "succeeded"
                }
            );
            assert_eq!(run.error.as_deref(), error);
            let events = database.events("strict", 0, 100).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, "ended");
        }
    }

    #[test]
    fn repeat_options_roundtrip() {
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
