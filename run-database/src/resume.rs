//! What a resumed run needs from the runs before it: the lineage it joins, and the outcomes of
//! the tasks that already finished, by fingerprint.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};
use workflow_graph::{Outcome, OutputFile, TaskId};

use crate::{Database, DatabaseError, Run};

/// The outcome of a finished task of an earlier run, which a resumed run can take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reusable {
    pub run: String,
    pub outcome: Outcome,
}

impl Database {
    /// The latest run of a workflow, or with `real`, its latest run that was not simulated.
    pub fn last_run(&self, workflow: &str, real: bool) -> Result<Option<Run>, DatabaseError> {
        let id: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM runs WHERE workflow = ?1 AND (simulated = 0 OR NOT ?2)
                 ORDER BY started DESC, rowid DESC LIMIT 1",
                params![workflow, real],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.run(&id),
            None => Ok(None),
        }
    }

    /// The ids of the runs of a lineage, the latest first.
    pub fn lineage(&self, lineage: &str) -> Result<Vec<String>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM runs WHERE lineage = ?1 ORDER BY started DESC, rowid DESC")?;
        let ids = statement
            .query_map([lineage], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// The outcomes of the tasks that succeeded in the given runs, by fingerprint; for a
    /// fingerprint in several runs, the run that comes first in the list wins. A task whose
    /// standard output or error the database does not hold in whole is left out, since its
    /// driver may read them.
    pub fn reusable(&self, runs: &[String]) -> Result<HashMap<String, Reusable>, DatabaseError> {
        let mut reusable = HashMap::new();
        for run in runs {
            let mut statement = self.connection.prepare(
                "SELECT t.id, t.fingerprint, a.number FROM tasks t JOIN attempts a
                    ON a.run = t.run AND a.task = t.id
                 WHERE t.run = ?1 AND t.state = 'succeeded' AND a.exit_code = 0
                    AND a.number = (SELECT MAX(number) FROM attempts
                        WHERE run = t.run AND task = t.id)",
            )?;
            let finished = statement
                .query_map([run], |row| {
                    Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?, row.get(2)?))
                })?
                .collect::<Result<Vec<(u64, String, u32)>, _>>()?;
            for (task, fingerprint, attempt) in finished {
                if reusable.contains_key(&fingerprint) {
                    continue;
                }
                if let Some(outcome) = self.outcome(run, task, attempt)? {
                    let run = run.clone();
                    reusable.insert(fingerprint, Reusable { run, outcome });
                }
            }
        }
        Ok(reusable)
    }

    /// The outcome of an attempt as its driver heard it, when the database holds all of it.
    fn outcome(
        &self,
        run: &str,
        task: u64,
        attempt: u32,
    ) -> Result<Option<Outcome>, DatabaseError> {
        let text = |stream| -> Result<Option<String>, DatabaseError> {
            Ok(match self.log(run, task, attempt, stream)? {
                Some(log) if !log.whole => None,
                Some(log) => Some(log.text),
                None => Some(String::new()),
            })
        };
        let (Some(stdout), Some(stderr)) = (text("stdout")?, text("stderr")?) else {
            return Ok(None);
        };
        let files = self
            .files(run, Some(task))?
            .into_iter()
            .filter(|file| file.attempt == Some(attempt))
            .map(|file| OutputFile {
                path: file.path,
                location: PathBuf::from(file.location),
                directory: file.directory,
            })
            .collect();
        let image: Option<String> = self.connection.query_row(
            "SELECT image FROM attempts WHERE run = ?1 AND task = ?2 AND number = ?3",
            params![run, task, attempt],
            |row| row.get(0),
        )?;
        // The result of an earlier run answers the task, so it does not run.
        Ok(Some(Outcome {
            id: TaskId(task),
            exit_code: 0,
            files,
            stdout,
            stderr,
            cached: true,
            image,
            ..Outcome::default()
        }))
    }
}
