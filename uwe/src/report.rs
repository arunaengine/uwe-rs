//! Writes a standalone HTML report for a recorded or newly completed run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::Database;

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
    let output =
        (arguments.output.clone()).unwrap_or_else(|| PathBuf::from(format!("{run}-report.html")));
    let size = write(&database, &run, &output)?;
    println!(
        "wrote the report of run {run} to {} ({})",
        output.display(),
        size_text(size)
    );
    Ok(())
}

/// Writes the report of a run that ended to the file of `-report`, when there is one, and gives
/// its path. A failure is only a warning, since the run itself is over.
pub fn after_run(arguments: &Arguments, run: &str) -> Option<PathBuf> {
    let file = arguments.report.as_ref()?;
    let written = runs::open(arguments).and_then(|database| write(&database, run, file));
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
fn write(database: &Database, run: &str, file: &Path) -> Result<u64, Failure> {
    let html = run_page::report(database, run)
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
    #[test]
    fn sizes_are_short() {
        assert_eq!(super::size_text(83_200), "84 KB");
        assert_eq!(super::size_text(2_450_000), "2.5 MB");
    }
}
