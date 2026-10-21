//! Reads back where the results of a run went.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::Serialize;

use crate::{Database, DatabaseError};

/// A result of a run and where it went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Delivered {
    /// A path on this machine or an address of a storage provider.
    pub target: String,
    /// The output that went there, by its location in the store.
    pub location: String,
    /// The task that made the output, when the run recorded it.
    pub task: Option<u64>,
}

impl Database {
    /// Where the results of every run went, like the links to the store that clean-up keeps.
    pub fn all_deliveries(&self) -> Result<Vec<Delivered>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT target, location FROM deliveries ORDER BY rowid")?;
        let found = statement
            .query_map([], |row| {
                Ok(Delivered {
                    target: row.get(0)?,
                    location: row.get(1)?,
                    task: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(found)
    }

    /// Where the results of a run went, in the order they went there.
    pub fn deliveries(&self, run: &str) -> Result<Vec<Delivered>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT d.target, d.location,
                (SELECT MIN(f.task) FROM files f
                 WHERE f.run = d.run AND f.role = 'output' AND f.location = d.location)
             FROM deliveries d WHERE d.run = ?1 ORDER BY d.rowid",
        )?;
        let found = statement
            .query_map([run], |row| {
                Ok(Delivered {
                    target: row.get(0)?,
                    location: row.get(1)?,
                    task: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(found)
    }
}
