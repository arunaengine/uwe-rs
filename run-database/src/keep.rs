//! What clean-up of a store asks the run database: which runs go on, which runs started lately,
//! and which files and contents runs refer to.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use rusqlite::params;

use crate::{Database, DatabaseError, Run};

/// The files and contents that runs refer to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct References {
    /// Where each input and output file of their tasks is: a local path or an address.
    pub locations: BTreeSet<String>,
    /// The digests of files and logs, like `sha256:<hex>`, where the database knows them.
    pub digests: BTreeSet<String>,
}

impl Database {
    /// Runs whose recorded process still runs, or whose foreign/legacy liveness is unknown.
    pub fn running_runs(&self) -> Result<Vec<Run>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM runs WHERE state = 'running' ORDER BY started")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut running = Vec::new();
        for id in ids {
            // A run whose process is gone reads as stopped.
            if let Some(run) = self.run(&id)?.filter(|run| run.state == "running") {
                running.push(run);
            }
        }
        Ok(running)
    }

    /// The ids of the runs that started at or after `time`, in milliseconds since the Unix
    /// epoch.
    pub fn runs_since(&self, time: i64) -> Result<Vec<String>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM runs WHERE started >= ?1 ORDER BY started")?;
        let ids = statement
            .query_map([time], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// The files and contents that the runs refer to: the inputs, outputs and logs of their
    /// tasks, the files of their workflows and command lines, and the remote files they read.
    pub fn references(&self, runs: &[String]) -> Result<References, DatabaseError> {
        let mut references = References::default();
        let mut files = self
            .connection
            .prepare("SELECT location, digest FROM files WHERE run = ?1")?;
        let mut contents = self.connection.prepare(
            "SELECT digest FROM logs WHERE run = ?1
             UNION SELECT digest FROM sources WHERE run = ?1
             UNION SELECT digest FROM remote_files WHERE run = ?1",
        )?;
        for run in runs {
            let rows = files.query_map(params![run], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (location, digest) = row?;
                if !location.is_empty() {
                    references.locations.insert(location);
                }
                references.digests.extend(digest);
            }
            let rows = contents.query_map(params![run], |row| row.get::<_, String>(0))?;
            for digest in rows {
                references.digests.insert(digest?);
            }
        }
        Ok(references)
    }
}
