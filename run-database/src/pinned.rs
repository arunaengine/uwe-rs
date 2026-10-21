//! Reads retained run sources, remote files, images and reproducibility verdicts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Database, DatabaseError};

/// A file that makes up the workflow of a run, or that its command line names, by its digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Source {
    pub kind: SourceKind,
    /// A file of the workflow relative to its folder; a named file as an absolute path.
    pub path: String,
    /// `sha256:<hex>`.
    pub digest: String,
    pub size: u64,
}

/// Why a file is a source of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A file of the folder of the workflow.
    Workflow,
    /// A file that the command line names, like a config file, a params file or a job file.
    Named,
}

impl SourceKind {
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::Workflow => "workflow",
            SourceKind::Named => "named",
        }
    }
}

/// A remote file that a run read: a task input, or a file that workflow code read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteFile {
    pub address: String,
    /// `sha256:<hex>` of the content the run read.
    pub digest: String,
    pub size: u64,
    /// When the content was read from the address, in milliseconds since the Unix epoch.
    pub fetched: i64,
    /// The version that the provider gave then, like an ETag, when known.
    pub version: Option<String>,
}

/// An image that tasks of a run named, and the digest it ran at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImagePin {
    /// The image as the workflow named it, like `debian:stable-slim`.
    pub named: String,
    /// The image as it ran, pinned to its digest, like `debian@sha256:...`; the name alone
    /// when the image has no digest, as one built on this machine.
    pub ran: String,
}

impl ImagePin {
    /// Whether the image ran at a digest.
    pub fn pinned(&self) -> bool {
        self.ran.contains("@sha256:")
    }
}

/// The verdict of the reproducibility check of an attempt: the task, whose result came from
/// the cache, ran again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckRow {
    pub task: u64,
    pub attempt: u32,
    /// The run that made the result in the cache.
    pub earlier: String,
    /// `same`, `differs` or `failed`.
    pub verdict: String,
    /// The outputs whose digests differ.
    pub differing: Vec<String>,
    /// The exit code of a task that failed when it ran again.
    pub exit_code: Option<i32>,
}

impl SourceKind {
    fn parse(name: &str) -> SourceKind {
        match name {
            "named" => SourceKind::Named,
            _ => SourceKind::Workflow,
        }
    }
}

impl Database {
    /// The files of the workflow of a run and the files its command line named, in the order
    /// they were recorded.
    pub fn sources(&self, run: &str) -> Result<Vec<Source>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT kind, path, digest, size FROM sources WHERE run = ?1 ORDER BY rowid",
        )?;
        let sources = statement
            .query_map([run], |row| {
                Ok(Source {
                    kind: SourceKind::parse(&row.get::<_, String>(0)?),
                    path: row.get(1)?,
                    digest: row.get(2)?,
                    size: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sources)
    }

    /// The remote files that a run read, by address.
    pub fn remote_files(&self, run: &str) -> Result<Vec<RemoteFile>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT address, digest, size, fetched, version FROM remote_files WHERE run = ?1
             ORDER BY address",
        )?;
        let files = statement
            .query_map([run], |row| {
                Ok(RemoteFile {
                    address: row.get(0)?,
                    digest: row.get(1)?,
                    size: row.get(2)?,
                    fetched: row.get(3)?,
                    version: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(files)
    }

    /// Each image that tasks of a run named, with the image it ran as in the last attempt that
    /// ran in it, by name.
    pub fn images(&self, run: &str) -> Result<Vec<ImagePin>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT json_extract(t.description, '$.image'), a.image
             FROM attempts a JOIN tasks t ON t.run = a.run AND t.id = a.task
             WHERE a.run = ?1 AND a.image IS NOT NULL
                AND json_extract(t.description, '$.image') IS NOT NULL
             ORDER BY a.started",
        )?;
        let rows = statement.query_map([run], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut images: Vec<ImagePin> = Vec::new();
        for row in rows {
            let (named, ran) = row?;
            match images.iter_mut().find(|image| image.named == named) {
                Some(image) => image.ran = ran,
                None => images.push(ImagePin { named, ran }),
            }
        }
        images.sort_by(|a, b| a.named.cmp(&b.named));
        Ok(images)
    }

    /// The JSON Schema of the outputs of the workflow of a run, when its driver described them.
    pub fn output_schema(&self, run: &str) -> Result<Option<serde_json::Value>, DatabaseError> {
        let text: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT output_schema FROM runs WHERE id = ?1",
                [run],
                |row| row.get(0),
            )
            .optional()?;
        match text.flatten() {
            Some(text) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    /// The verdicts of the reproducibility check in a run, or of one of its tasks.
    pub fn checks(&self, run: &str, task: Option<u64>) -> Result<Vec<CheckRow>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT task, attempt, earlier, verdict, differing, exit_code FROM checks
             WHERE run = ?1 AND (?2 IS NULL OR task = ?2) ORDER BY task, attempt",
        )?;
        let rows = statement
            .query_map(params![run, task], |row| {
                Ok((
                    CheckRow {
                        task: row.get(0)?,
                        attempt: row.get(1)?,
                        earlier: row.get(2)?,
                        verdict: row.get(3)?,
                        differing: Vec::new(),
                        exit_code: row.get(5)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(mut check, differing)| {
                check.differing = serde_json::from_str(&differing)?;
                Ok(check)
            })
            .collect()
    }
}
