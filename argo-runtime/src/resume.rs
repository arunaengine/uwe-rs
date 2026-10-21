//! Takes the requests of `iunctus resume` for suspended steps, as `argo resume` and `argo node set` give them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::run::{Run, display_name};

/// How often a run looks for requests while a step is suspended.
pub(crate) const POLL: Duration = Duration::from_secs(1);

/// A request to resume suspended steps: one by its name, or all; with the values of their
/// output parameters, as `argo node set --output-parameter` gives them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub step: Option<String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

impl Request {
    /// Leaves the request in the folder of a run, under a name that sorts by time.
    pub fn write(&self, folder: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(folder)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let partial = folder.join(format!("{stamp}.part"));
        std::fs::write(
            &partial,
            serde_json::to_vec(self).map_err(std::io::Error::other)?,
        )?;
        // Renamed when complete, so that a run never reads half of it.
        std::fs::rename(partial, folder.join(format!("{stamp}.json")))
    }
}

/// The requests of a run for its child workflow `name`: by the name of the child, which
/// resumes all of it as `argo resume` does, or by the full name or ID of one of its steps,
/// which start with the name of the child.
pub(crate) fn scoped(requests: &[Request], name: &str) -> Vec<Request> {
    let inside = |step: &str| {
        step.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(['.', '-', '[', '(']))
    };
    (requests.iter())
        .filter_map(|request| match request.step.as_deref() {
            Some(step) if step == name => Some(Request {
                step: None,
                outputs: request.outputs.clone(),
            }),
            Some(step) if inside(step) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

impl Run {
    /// Takes the requests that arrived; like `argo resume`, they apply to the steps that are
    /// suspended now, and go away.
    pub(crate) fn take_requests(&mut self) {
        // Child workflows get their requests from the run that made them.
        if self.nesting > 0 {
            return;
        }
        let Some(folder) = self.options.resume.clone() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&folder) else {
            return;
        };
        let mut paths: Vec<_> = (entries.flatten().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        paths.sort();
        for path in paths {
            let request = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Request>(&bytes).ok());
            let _ = std::fs::remove_file(&path);
            match request {
                Some(request) => self.requests.push(request),
                None => self.warn(format!(
                    "{} is no resume request; it is left out",
                    path.display()
                )),
            }
        }
    }

    /// The request that resumes a suspended node, by its name as Argo shows it, its full
    /// name or its ID.
    pub(crate) fn request_for(&self, index: usize) -> Option<Request> {
        let node = &self.nodes[index];
        let display = display_name(&node.name);
        self.requests
            .iter()
            .find(|request| match request.step.as_deref() {
                None => true,
                Some(step) => step == display || step == node.name || step == node.id,
            })
            .cloned()
    }

    /// The step as `iunctus resume` names it: its display name, or its full name in a child
    /// workflow, whose display names may be those of steps of the parent.
    pub(crate) fn resume_name(&self, index: usize) -> String {
        let name = &self.nodes[index].name;
        match self.nesting {
            0 => display_name(name),
            _ => name.clone(),
        }
    }

    /// The run as `iunctus resume` names it: the folder of its requests.
    pub(crate) fn run_name(&self) -> String {
        let folder = self.options.resume.as_deref();
        let name = folder
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy());
        name.map(|name| name.into_owned()).unwrap_or_default()
    }
}
