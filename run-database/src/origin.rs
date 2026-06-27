//! Where a file came from: the task that made it, the tasks that made that task's inputs, and so
//! on back to the data that no task made. The chain can cross runs, since a resumed run reads
//! the files of the runs before it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Database, DatabaseError, FileRow};

/// The most tasks that one answer lists, so that a file of a huge run gives a short answer.
const MOST: usize = 1000;

/// One task in the chain that made a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Origin {
    /// How many tasks lie between it and the file: 0 for the task that made the file.
    pub depth: usize,
    pub run: String,
    pub task: u64,
    pub name: String,
    pub step: String,
    /// The image as it ran, or as the workflow named it.
    pub image: Option<String>,
    /// The inputs of the task that no task made: data from outside the runs, or literal
    /// content that the workflow wrote. The other inputs have entries of their own.
    pub data: Vec<FileRow>,
}

impl Database {
    /// The chain of tasks that made the file at a location, or with a digest like
    /// `sha256:<hex>`, from the task that made it back to data that no task made. The latest
    /// run that made the file answers. Empty when no run made it.
    pub fn origin(&self, file: &str) -> Result<Vec<Origin>, DatabaseError> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut pending = vec![(file.to_owned(), 0)];
        while let Some((file, depth)) = pending.pop() {
            if chain.len() >= MOST {
                break;
            }
            let Some((run, task)) = self.maker(&file)? else {
                continue;
            };
            if !seen.insert((run.clone(), task)) {
                continue;
            }
            let Some(detail) = self.task(&run, task)? else {
                continue;
            };
            let mut data = Vec::new();
            for input in detail.inputs.iter().rev() {
                if !input.location.is_empty() && self.maker(&input.location)?.is_some() {
                    pending.push((input.location.clone(), depth + 1));
                } else {
                    data.insert(0, input.clone());
                }
            }
            let image = detail
                .attempts
                .last()
                .and_then(|attempt| attempt.image.clone());
            chain.push(Origin {
                depth,
                image: image.or(detail.description.image),
                name: detail.row.name,
                step: detail.row.step,
                data,
                run,
                task,
            });
        }
        Ok(chain)
    }

    /// The run and task that made a file, by its location or its digest; the latest run wins.
    fn maker(&self, file: &str) -> Result<Option<(String, u64)>, DatabaseError> {
        let found = self
            .connection
            .query_row(
                "SELECT f.run, f.task FROM files f JOIN runs r ON r.id = f.run
                 WHERE f.role = 'output' AND (f.location = ?1 OR f.digest = ?1)
                 ORDER BY r.started DESC, f.rowid DESC LIMIT 1",
                params![file],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(found)
    }
}
