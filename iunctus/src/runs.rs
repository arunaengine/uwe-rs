//! The commands that read the run database: `iunctus runs` lists the runs, and
//! `iunctus export <run>` writes the Workflow Run RO-Crate of a run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use run_database::{Database, Run};
use workflow_graph::Language;

use crate::arguments::Arguments;
use crate::{Failure, cache_directory, detect, failure};

/// How many runs `iunctus runs` lists.
const LISTED: u32 = 50;

/// The run database of the command: `-database`, else `runs.sqlite` in the cache directory,
/// next to the default store.
pub fn database_path(arguments: &Arguments) -> PathBuf {
    (arguments.database.clone()).unwrap_or_else(|| cache_directory().join(run_database::FILE_NAME))
}

pub fn open(arguments: &Arguments) -> Result<Database, Failure> {
    Database::open(&database_path(arguments)).map_err(|error| failure(error.to_string()))
}

/// `iunctus runs`: the latest runs, one line each.
pub fn list(arguments: &Arguments) -> Result<(), Failure> {
    let runs = open(arguments)?
        .runs(LISTED)
        .map_err(|error| failure(error.to_string()))?;
    print!("{}", table(&runs));
    Ok(())
}

/// The runs as a table with a header line.
fn table(runs: &[Run]) -> String {
    let mut text = format!(
        "{:<20} {:<19} {:<9} {:<9} {:>6} {:>6}  WORKFLOW\n",
        "RUN", "STARTED", "STATE", "EXECUTOR", "TASKS", "FAILED"
    );
    for run in runs {
        let tasks: u64 = run.tasks.values().sum();
        let failed = run.tasks.get("failed").copied().unwrap_or_default();
        let executor = if run.simulated {
            "simulated"
        } else {
            run.executor.as_str()
        };
        let _ = writeln!(
            text,
            "{:<20} {:<19} {:<9} {:<9} {:>6} {:>6}  {}",
            run.id,
            local_time(run.started),
            run.state,
            executor,
            tasks,
            failed,
            run.workflow
        );
    }
    text
}

/// A time of the database in the local time zone, to the second.
pub fn local_time(milliseconds: i64) -> String {
    jiff::Timestamp::from_millisecond(milliseconds)
        .map(|time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// `iunctus export <run>`: writes the RO-Crate of the run to `-output`, by default to
/// `<run>-crate` in the current directory.
pub fn export(arguments: &Arguments) -> Result<(), Failure> {
    let Some(id) = arguments.target.as_ref().map(|id| id.to_string_lossy()) else {
        return Err(failure(
            "iunctus: name the run to export; `iunctus runs` lists them",
        ));
    };
    let database = open(arguments)?;
    let Some(run) = database
        .run(&id)
        .map_err(|error| failure(error.to_string()))?
    else {
        return Err(failure(format!(
            "iunctus: the run database has no run `{id}`; `iunctus runs` lists the runs"
        )));
    };
    let workflow = main_file(&run)?;
    let output = (arguments.output.clone()).unwrap_or_else(|| PathBuf::from(format!("{id}-crate")));
    let options = run_crate::Options {
        workflow,
        license: arguments.license.clone(),
    };
    let summary = run_crate::export(&database, &id, &output, &options)
        .map_err(|error| failure(error.to_string()))?;
    let kind = if run.simulated {
        "simulated run"
    } else {
        "run"
    };
    println!(
        "wrote the RO-Crate of the {kind} {id} to {}: {} tasks, {} files ({} bytes)",
        output.display(),
        summary.tasks,
        summary.files,
        summary.bytes
    );
    Ok(())
}

/// The main file of the workflow of a run, which the crate holds a copy of.
fn main_file(run: &Run) -> Result<PathBuf, Failure> {
    let target = Path::new(&run.workflow);
    let language = match run.language.as_str() {
        "nextflow" => Language::Nextflow,
        "snakemake" => Language::Snakemake,
        "cwl" => Language::Cwl,
        _ => Language::Argo,
    };
    detect::main_file(target, language).ok_or_else(|| {
        failure(format!(
            "iunctus: the workflow {} of the run is gone, so the crate cannot hold it",
            target.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn runs_are_listed_one_per_line() {
        let run = Run {
            id: "19a8f3c2b10-1a2b".into(),
            lineage: "19a8f3c2b10-1a2b".into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: "nextflow".into(),
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "simulator".into(),
            simulated: true,
            store: None,
            process: 1,
            started: 0,
            ended: None,
            state: "succeeded".into(),
            error: None,
            tasks: BTreeMap::from([("failed".into(), 1), ("succeeded".into(), 4)]),
        };
        let text = table(&[run]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("RUN "), "{text}");
        let words: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(words[0], "19a8f3c2b10-1a2b");
        assert_eq!(
            &words[3..],
            ["succeeded", "simulated", "5", "1", "/work/main.nf"]
        );
    }
}
