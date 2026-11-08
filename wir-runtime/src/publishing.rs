//! The files that stream ops write for a folder outside the run: they stay in the folder of the
//! op in the file area, and the host delivers them to a folder that a publish effect names.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{Value as Json, json};
use wir::Effect;
use workflow_graph::{Delivery, DeliveryMode};

use crate::engine::Context;
use crate::operator::Turn;
use crate::streams::Failure;

/// The deliveries of stream ops: asked by ops, handed to the host, and acknowledged by it.
#[derive(Debug, Default)]
pub struct Deliveries(Mutex<Shelves>);

#[derive(Debug, Default)]
struct Shelves {
    asked: Vec<Delivery>,
    open: Vec<Delivery>,
    done: Vec<Delivery>,
}

impl Deliveries {
    fn shelves(&self) -> MutexGuard<'_, Shelves> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The deliveries that ops asked for since the last call, for the host.
    pub fn take(&self) -> Vec<Delivery> {
        let mut shelves = self.shelves();
        let asked = std::mem::take(&mut shelves.asked);
        shelves.open.extend_from_slice(&asked);
        asked
    }

    /// Takes the acknowledgement of a delivery; whether an op asked for it.
    pub fn acknowledge(&self, delivery: &Delivery) -> bool {
        let mut shelves = self.shelves();
        let Some(at) = shelves.open.iter().position(|open| open == delivery) else {
            return false;
        };
        let done = shelves.open.remove(at);
        shelves.done.push(done);
        true
    }

    /// Whether the host acknowledged every one of `deliveries`, which then leave.
    fn settled(&self, deliveries: &[Delivery]) -> bool {
        let mut shelves = self.shelves();
        let mut done = shelves.done.clone();
        for delivery in deliveries {
            match done.iter().position(|item| item == delivery) {
                Some(at) => done.remove(at),
                None => return false,
            };
        }
        shelves.done = done;
        true
    }
}

/// Where an op writes the files of its `directory`: a folder in its part of the file area, and
/// the outside folder that the host delivers them to, which a publish effect must name.
pub fn folder(
    turn: &Turn<'_>,
    context: &Context<'_>,
    effects: &[Effect],
    directory: Option<&str>,
) -> Result<(PathBuf, Option<String>), Failure> {
    let own = context.files.join(&turn.path);
    let Some(directory) = directory else {
        return Ok((own, None));
    };
    if directory.starts_with('/') || directory.contains("://") {
        if !wir::plain_folder(directory) {
            return Err(turn.failed(format!("the folder {directory} has an empty, . or .. part")));
        }
        return match effects.iter().any(|effect| effect.publishes(directory)) {
            true => Ok((own, Some(directory.trim_end_matches('/').to_owned()))),
            false => Err(turn.failed(format!(
                "the folder {directory} needs a publish effect that names it"
            ))),
        };
    }
    if directory.split('/').any(|part| part == "..") {
        return Err(turn.failed(format!("the folder {directory} leaves the file area")));
    }
    Ok((own.join(directory), None))
}

/// The deliveries of the files of an op, which it gives once the host acknowledged them.
#[derive(Debug, Default)]
pub struct Publication(Vec<Delivery>);

impl Publication {
    /// The File of a file written at `path`. For an outside folder it names the delivered file,
    /// unless the run has no files provider and so delivers nothing.
    pub fn file(
        &mut self,
        context: &Context<'_>,
        path: &Path,
        (name, size): (&str, usize),
        outside: Option<&str>,
    ) -> Json {
        let mut location = path.to_string_lossy().into_owned();
        if let Some(outside) = outside {
            let target = format!("{outside}/{name}");
            self.0.push(Delivery {
                location: location.clone(),
                target: target.clone(),
                mode: DeliveryMode::Copy,
                overwrite: true,
            });
            if context.provider.is_some() {
                location = target;
            }
        }
        json!({"class": "File", "location": location, "basename": name, "size": size})
    }

    /// Asks the host for the deliveries.
    pub fn ask(&self, context: &Context<'_>) {
        context
            .deliveries
            .shelves()
            .asked
            .extend_from_slice(&self.0);
    }

    /// Why the files wait, when they have deliveries: for the host to acknowledge them.
    pub fn awaited(&self) -> Option<String> {
        let target = &self.0.first()?.target;
        let folder = target
            .rsplit_once('/')
            .map_or(target.as_str(), |(folder, _)| folder);
        Some(format!(
            "the host never acknowledged the delivery of its files to {folder}"
        ))
    }

    /// Whether the files can be given: the host acknowledged their deliveries.
    pub fn delivered(&self, context: &Context<'_>) -> bool {
        self.0.is_empty() || context.provider.is_none() || context.deliveries.settled(&self.0)
    }
}
