//! Every run records itself in the run database (decision 10), simulated or real. This module
//! starts the record of a run, and finds the earlier run that `-resume` continues.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use run_database::{Database, Recorder, Reusable, Run, RunStart};
use workflow_graph::Language;

use crate::arguments::{Arguments, Resume};
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
    pub store: Option<&'a Path>,
}

/// Opens the run database and starts the record of a run. With `-resume`, the run joins the
/// lineage of the run it continues, and gets the finished tasks of that lineage.
pub fn start(arguments: &Arguments, run: &Started) -> Result<Recording, Failure> {
    let broken = |error: run_database::DatabaseError| failure(format!("iunctus: {error}"));
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
        arguments: std::env::args().skip(1).collect(),
        parameters: arguments.params.clone(),
        executor: run.executor.to_owned(),
        simulated: run.simulated,
        store: run.store.map(|store| store.to_string_lossy().into_owned()),
    };
    let recorder = database.record(&start).map_err(broken)?;
    Ok(Recording { recorder, reusable })
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
