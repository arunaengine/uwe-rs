//! Writes a standalone HTML report for a recorded or newly completed run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::Database;
use workflow_graph::Files;

use crate::arguments::Arguments;
use crate::{Failure, failure, runs};

/// `uwe report <run>`: writes the report of the run to `-output`, by default
/// `<run>-report.html`.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(run) = arguments.target.as_ref().map(|run| run.to_string_lossy()) else {
        return Err(Failure::usage(
            "uwe: name a run, like `uwe report 1a0f1cc4c10-3a2487`; `uwe runs` lists \
             the runs",
        ));
    };
    let database = runs::open(arguments)?;
    let setup = crate::setup::Setup::load(arguments)?;
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let files =
        task_runner::StorageFiles::new(&setup.storage, crate::cache_directory().join("downloads"));
    let output =
        (arguments.output.clone()).unwrap_or_else(|| PathBuf::from(format!("{run}-report.html")));
    let size = write(&database, &run, &output, Some(&files))?;
    println!(
        "wrote the report of run {run} to {} ({})",
        output.display(),
        size_text(size)
    );
    Ok(())
}

/// Writes the report of a run that ended to the file of `-report`, when there is one, and gives
/// its path. A failure is only a warning, since the run itself is over.
pub fn after_run(arguments: &Arguments, run: &str, files: Option<&dyn Files>) -> Option<PathBuf> {
    let file = arguments.report.as_ref()?;
    let written = runs::open(arguments).and_then(|database| write(&database, run, file, files));
    match written {
        Ok(_) => Some(std::path::absolute(file).unwrap_or_else(|_| file.clone())),
        Err(problem) => {
            tracing::warn!(
                "uwe: the report of the run was not written: {}",
                problem.message.trim_start_matches("uwe: ")
            );
            None
        }
    }
}

/// Writes the report of a run to a file, and gives its size in bytes.
fn write(
    database: &Database,
    run: &str,
    file: &Path,
    files: Option<&dyn Files>,
) -> Result<u64, Failure> {
    let html =
        run_page::report_with_files(database, run, files.unwrap_or(&workflow_graph::LocalFiles))
            .map_err(|error| failure(format!("uwe: {error}")))?
            .ok_or_else(|| {
                failure(format!(
                    "uwe: the run database has no run `{run}`; `uwe runs` lists the runs"
                ))
            })?;
    let cannot =
        |error: std::io::Error| failure(format!("uwe: cannot write {}: {error}", file.display()));
    if let Some(folder) = file
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        std::fs::create_dir_all(folder).map_err(cannot)?;
    }
    std::fs::write(file, &html).map_err(cannot)?;
    Ok(html.len() as u64)
}

/// A size in kilobytes or megabytes, like `84 KB`.
fn size_text(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1000)),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_configured_logs() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"configured retained message\n";
        std::fs::write(root.path().join("stdout"), bytes).unwrap();
        let config = config_file::ConfigFile::parse(
            &format!(
                "[storage.logs]\ntype='folder'\nprefixes='s3://private/'\npath='{}'",
                root.path().display()
            ),
            &root.path().join("config.toml"),
        )
        .unwrap();
        let storage = storage::Storage::from_config(&config).unwrap();
        let files = task_runner::StorageFiles::new(&storage, root.path().join("downloads"));
        let path = root.path().join("runs.sqlite");
        let start = run_database::RunStart {
            id: "report".into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: workflow_graph::Language::Nextflow,
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
        let task = workflow_graph::Task {
            id: workflow_graph::TaskId(1),
            defer_logs: true,
            ..workflow_graph::Task::default()
        };
        recorder.requested(&task, &[]);
        recorder.started(task.id, 1);
        recorder.finished(
            &workflow_graph::Outcome {
                id: task.id,
                stdout_log: Some(workflow_graph::Stream {
                    location: "s3://private/stdout".into(),
                    digest: task_model::Digest::sha256(bytes).to_string(),
                    size: bytes.len() as u64,
                }),
                ..workflow_graph::Outcome::default()
            },
            1,
            false,
        );
        recorder.ended(None);
        recorder.close().unwrap();
        let output = root.path().join("report.html");
        write(
            &Database::open(&path).unwrap(),
            "report",
            &output,
            Some(&files),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(output)
                .unwrap()
                .contains("configured retained message")
        );
    }

    #[test]
    fn sizes_are_short() {
        assert_eq!(super::size_text(83_200), "84 KB");
        assert_eq!(super::size_text(2_450_000), "2.5 MB");
    }
}
