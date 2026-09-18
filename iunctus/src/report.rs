//! The report of a run, one HTML file that needs no server (decision 23's page in a file):
//! `iunctus report <run>` writes it for any run, and `iunctus run -report <file>`, or `file` of
//! the `[report]` section of the config file, writes it when the run ends. It suits machines
//! where no browser can reach the page, like the login node of a cluster: copy the file and
//! open it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::Database;

use crate::arguments::Arguments;
use crate::{Failure, failure, runs};

/// `iunctus report <run>`: writes the report of the run to `-output`, by default
/// `<run>-report.html`.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(run) = arguments.target.as_ref().map(|run| run.to_string_lossy()) else {
        return Err(Failure::usage(
            "iunctus: name a run, like `iunctus report 1a0f1cc4c10-3a2487`; `iunctus runs` lists \
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
                "iunctus: the report of the run was not written: {}",
                problem.message.trim_start_matches("iunctus: ")
            );
            None
        }
    }
}

/// Writes the report of a run to a file, and gives its size in bytes.
fn write(database: &Database, run: &str, file: &Path) -> Result<u64, Failure> {
    let html = run_page::report(database, run)
        .map_err(|error| failure(format!("iunctus: {error}")))?
        .ok_or_else(|| {
            failure(format!(
                "iunctus: the run database has no run `{run}`; `iunctus runs` lists the runs"
            ))
        })?;
    let cannot = |error: std::io::Error| {
        failure(format!("iunctus: cannot write {}: {error}", file.display()))
    };
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
