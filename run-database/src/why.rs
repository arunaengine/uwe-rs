//! Why a task ran instead of taking its result from an earlier run: the part of its
//! description that changed since the same task ran before, in plain words.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use workflow_graph::Task;

use crate::{Database, DatabaseError, FileRow, TaskDetail};

/// What differs between a task and the same task of an earlier run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Why {
    /// The earlier run, and the same task there.
    pub run: String,
    pub task: u64,
    /// One sentence per difference, like "the input `b.txt` changed: 6 bytes before, 10 now".
    pub changes: Vec<String>,
}

impl Database {
    /// Why a task of a run ran: what differs from the same task in the run that this run
    /// resumes, or else in the latest earlier run of the same workflow. `None` when no earlier
    /// run has a task of the same step.
    pub fn why(&self, run: &str, task: u64) -> Result<Option<Why>, DatabaseError> {
        let Some(now) = self.task(run, task)? else {
            return Ok(None);
        };
        let Some((earlier_run, earlier_task)) = self.earlier(run, &now)? else {
            return Ok(None);
        };
        let Some(before) = self.task(&earlier_run, earlier_task)? else {
            return Ok(None);
        };
        let mut changes = described(&before.description, &now.description);
        changes.extend(self.inputs(run, &before, &now)?);
        if changes.is_empty() {
            changes.push(same_task(&before, &now, self.resumes(run, &earlier_run)?));
        }
        Ok(Some(Why {
            run: earlier_run,
            task: earlier_task,
            changes,
        }))
    }

    /// Matches a prior task by step and fingerprint, then shared input locations, then name.
    /// See `docs/design/runs.md` for the full contract.
    fn earlier(&self, run: &str, now: &TaskDetail) -> Result<Option<(String, u64)>, DatabaseError> {
        let step = &now.row.step;
        let earlier: Option<String> = self
            .connection
            .query_row(
                "SELECT e.id FROM runs r JOIN runs e ON e.workflow = r.workflow
                 WHERE r.id = ?1 AND e.id != r.id AND e.started <= r.started
                    AND e.replays IS NULL
                    AND EXISTS (SELECT 1 FROM tasks t WHERE t.run = e.id AND t.step = ?2)
                 ORDER BY e.id = r.resumes DESC, e.started DESC, e.rowid DESC LIMIT 1",
                params![run, step],
                |row| row.get(0),
            )
            .optional()?;
        let Some(earlier) = earlier else {
            return Ok(None);
        };
        let sources: HashSet<(&str, &str)> = now
            .description
            .inputs
            .iter()
            .map(|input| (input.path.as_str(), input.source.as_str()))
            .collect();
        let mut statement = self.connection.prepare(
            "SELECT id, name, description, fingerprint FROM tasks
             WHERE run = ?1 AND step = ?2 ORDER BY rowid",
        )?;
        let candidates = statement.query_map(params![earlier, step], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut best: Option<(u64, usize)> = None;
        for candidate in candidates {
            let (id, name, description, fingerprint) = candidate?;
            let task: Task = serde_json::from_str(&description)?;
            let shared = task
                .inputs
                .iter()
                .filter(|input| sources.contains(&(input.path.as_str(), input.source.as_str())))
                .count();
            let score = usize::from(fingerprint == now.fingerprint) * 1_000_000
                + shared * 10
                + usize::from(name == now.row.name);
            if best.is_none_or(|(_, top)| score > top) {
                best = Some((id, score));
            }
        }
        Ok(best.map(|(id, _)| (earlier, id)))
    }

    /// Whether a run is in the lineage of an earlier run, so that it could take its results.
    fn resumes(&self, run: &str, earlier: &str) -> Result<bool, DatabaseError> {
        let same: bool = self.connection.query_row(
            "SELECT a.lineage = b.lineage FROM runs a, runs b WHERE a.id = ?1 AND b.id = ?2",
            params![run, earlier],
            |row| row.get(0),
        )?;
        Ok(same)
    }

    /// The changes of the inputs, by the path the task sees.
    fn inputs(
        &self,
        run: &str,
        before: &TaskDetail,
        now: &TaskDetail,
    ) -> Result<Vec<String>, DatabaseError> {
        let by_path = |files: &[FileRow]| -> BTreeMap<String, FileRow> {
            files
                .iter()
                .map(|file| (file.path.clone(), file.clone()))
                .collect()
        };
        let (before_files, now_files) = (by_path(&before.inputs), by_path(&now.inputs));
        let mut changes = Vec::new();
        for (path, file) in &now_files {
            let Some(old) = before_files.get(path) else {
                changes.push(format!("the input `{path}` is new"));
                continue;
            };
            if old.location == file.location && old.size == file.size && old.digest == file.digest {
                continue;
            }
            if old.digest.is_some() && file.digest.is_some() && old.digest != file.digest {
                changes.push(format!("the content of the input `{path}` changed"));
            } else if old.size != file.size {
                changes.push(format!(
                    "the input `{path}` changed: {} before, {} now",
                    size(old.size),
                    size(file.size)
                ));
            } else if let Some(producer) = self.producer(run, &file.location)? {
                changes.push(format!(
                    "the input `{path}` is a new output of task {producer}, which ran again"
                ));
            } else if old.location != file.location {
                changes.push(format!(
                    "the input `{path}` comes from {} now, from {} before",
                    file.location, old.location
                ));
            }
        }
        for path in before_files
            .keys()
            .filter(|path| !now_files.contains_key(*path))
        {
            changes.push(format!("the input `{path}` is gone"));
        }
        Ok(changes)
    }

    /// The task of a run that made the file at a location, if one did.
    fn producer(&self, run: &str, location: &str) -> Result<Option<u64>, DatabaseError> {
        let found = self
            .connection
            .query_row(
                "SELECT task FROM files WHERE run = ?1 AND location = ?2 AND role = 'output'
                 LIMIT 1",
                params![run, location],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }
}

/// The changes of what the driver described, besides the inputs.
pub(crate) fn described(before: &Task, now: &Task) -> Vec<String> {
    let mut changes = Vec::new();
    if before.image != now.image {
        changes.push(format!(
            "the image changed from {} to {}",
            shown(before.image.as_deref()),
            shown(now.image.as_deref())
        ));
    }
    if before.command != now.command {
        changes.push(format!(
            "the command changed from `{}` to `{}`",
            before.command.join(" "),
            now.command.join(" ")
        ));
    }
    if before.script != now.script {
        changes.push("the script changed".to_owned());
    }
    if before.stdin != now.stdin {
        changes.push("the standard input changed".to_owned());
    }
    if before.workdir != now.workdir {
        changes.push("the work directory changed".to_owned());
    }
    if before.outputs != now.outputs {
        changes.push(format!(
            "the outputs changed from {} to {}",
            before.outputs.join(", "),
            now.outputs.join(", ")
        ));
    }
    for (name, value) in &now.environment {
        match before.environment.get(name) {
            Some(old) if old == value => {}
            Some(old) => changes.push(format!(
                "the environment variable {name} changed from `{old}` to `{value}`"
            )),
            None => changes.push(format!("the environment variable {name} is new")),
        }
    }
    for name in before.environment.keys() {
        if !now.environment.contains_key(name) {
            changes.push(format!("the environment variable {name} is gone"));
        }
    }
    let inputs = |task: &Task| -> BTreeMap<String, Option<String>> {
        let literal = task.inputs.iter().filter(|input| input.content.is_some());
        literal
            .map(|input| (input.path.clone(), input.content.clone()))
            .collect()
    };
    let (old, new) = (inputs(before), inputs(now));
    for (path, content) in &new {
        if old.get(path).is_some_and(|old| old != content) {
            changes.push(format!(
                "the content of `{path}`, which the workflow writes, changed"
            ));
        }
    }
    changes
}

/// Why the same task with the same inputs ran again.
fn same_task(before: &TaskDetail, now: &TaskDetail, resumed: bool) -> String {
    let run = |detail: &TaskDetail| detail.row.state.clone();
    if before.row.state != "succeeded" {
        return format!("nothing changed, but the task {} before", run(before));
    }
    if before.fingerprint != now.fingerprint {
        return "an input file was written again: its size is the same, its time is new".into();
    }
    if !resumed {
        return "nothing changed, but this run did not resume the earlier one; `-resume` takes \
                its results"
            .into();
    }
    "nothing changed, but the files of the earlier result were gone".into()
}

fn shown(image: Option<&str>) -> String {
    image.map_or_else(|| "none".to_owned(), |image| format!("`{image}`"))
}

fn size(size: Option<u64>) -> String {
    size.map_or_else(|| "unknown".to_owned(), |size| format!("{size} bytes"))
}
