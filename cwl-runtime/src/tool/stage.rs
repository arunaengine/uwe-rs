//! Where every input file of a task goes inside the container: each file of the input object
//! in a folder of its own, with its secondary files beside it and the entries of a literal
//! directory inside it, and the InitialWorkDir entries in the output directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use workflow_graph::{Input, Writable};

use super::STAGING;
use crate::files;
use crate::value::{Map, Value};

/// Chooses where every input file goes inside the container.
#[derive(Default)]
pub(super) struct Stager {
    pub(super) inputs: Vec<Input>,
    /// Container paths by location.
    pub(super) paths: HashMap<String, String>,
    pub(super) folders: usize,
    /// The output directory, where relative task input paths are.
    pub(super) outdir: String,
    /// The locations of the files of the input object.
    pub(super) from_job: HashSet<String>,
    /// The locations that InitialWorkDir copies as writable.
    pub(super) writable: HashSet<String>,
    /// The writable entries that `InplaceUpdateRequirement` updates in place.
    pub(super) in_place: Vec<InPlace>,
}

/// A writable entry that the tool updates in place: its path in the output directory, where
/// the task's copy is collected, and the location it was staged from.
#[derive(Clone, Debug)]
pub struct InPlace {
    pub path: String,
    pub location: String,
    pub directory: bool,
}

impl Stager {
    /// Stages every file object of an input object, each top-level one in a folder of its own.
    pub(super) fn stage_job(&mut self, job: &Value) {
        match job {
            Value::List(items) => {
                for item in items {
                    self.stage_job(item);
                }
            }
            Value::Map(map) if job.is_file_like() => {
                let location = map
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.paths.contains_key(location) {
                    return;
                }
                let folder = format!("{STAGING}/{}", self.folders);
                self.folders += 1;
                self.stage(map, &folder, false);
                let mut file = job.clone();
                files::visit_files(&mut file, &mut |inner| {
                    if let Some(Value::String(location)) = inner.get("location") {
                        self.from_job.insert(location.clone());
                    }
                });
            }
            Value::Map(map) => {
                for (_, value) in map {
                    self.stage_job(value);
                }
            }
            _ => {}
        }
    }

    /// Stages a file object into a folder, with its secondary files beside it and the
    /// entries of a literal directory inside it. `replace` lets the new path win. The result
    /// is the path of the file object in the work directory.
    pub(super) fn stage(&mut self, file: &Map, folder: &str, replace: bool) -> String {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let basename = file
            .get("basename")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| files::basename_of(&location));
        let target = join(folder, &basename);
        if replace || !self.paths.contains_key(&location) {
            let inside = self.container_path(&target);
            self.paths.insert(location.clone(), inside);
        }
        let directory = file.get("class").and_then(Value::as_str) == Some("Directory");
        let literal = location.starts_with("_:");
        if directory {
            if literal {
                self.add(Input {
                    path: target.clone(),
                    source: String::new(),
                    directory: true,
                    content: None,
                    ..Default::default()
                });
            } else {
                self.add(Input {
                    path: target.clone(),
                    source: source_of(&location),
                    directory: true,
                    content: None,
                    ..Default::default()
                });
            }
            if let Some(Value::List(listing)) = file.get("listing") {
                for entry in listing {
                    let Value::Map(entry) = entry else { continue };
                    let child = entry
                        .get("location")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match child.strip_prefix(&format!("{location}/")) {
                        Some(relative) if !literal => {
                            self.inside(entry, &join(&target, relative), replace);
                        }
                        _ => {
                            self.stage(entry, &target, replace);
                        }
                    }
                }
            }
        } else if literal {
            let content = file
                .get("contents")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.add(Input {
                path: target.clone(),
                source: String::new(),
                directory: false,
                content: Some(content.to_owned()),
                ..Default::default()
            });
        } else {
            self.add(Input {
                path: target.clone(),
                source: source_of(&location),
                directory: false,
                content: None,
                ..Default::default()
            });
        }
        if let Some(Value::List(secondary)) = file.get("secondaryFiles") {
            for entry in secondary {
                if let Value::Map(entry) = entry {
                    self.stage(entry, folder, replace);
                }
            }
        }
        target
    }

    /// Lets the task change what is staged at `target` and inside it, in copies of its own,
    /// as an InitialWorkDir entry with `writable: true` asks.
    pub(super) fn make_writable(&mut self, target: &str) {
        let inside = format!("{}/", target.trim_end_matches('/'));
        for input in &mut self.inputs {
            if input.path == target || input.path.starts_with(&inside) {
                input.writable = Writable::Copy;
            }
        }
    }

    /// Records the path of an entry that a staged directory already holds.
    fn inside(&mut self, entry: &Map, target: &str, replace: bool) {
        let location = entry
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if replace || !self.paths.contains_key(location) {
            let inside = self.container_path(target);
            self.paths.insert(location.to_owned(), inside);
        }
        if let Some(Value::List(listing)) = entry.get("listing") {
            for child in listing {
                let Value::Map(child) = child else { continue };
                let name = child
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(relative) = name.strip_prefix(&format!("{location}/")) {
                    self.inside(child, &join(target, relative), replace);
                }
            }
        }
    }

    /// Task input paths are relative to the workdir, the output directory, or absolute.
    fn container_path(&self, target: &str) -> String {
        if target.starts_with('/') {
            target.to_owned()
        } else {
            format!("{}/{target}", self.outdir)
        }
    }

    fn add(&mut self, input: Input) {
        if let Some(existing) = self
            .inputs
            .iter_mut()
            .find(|existing| existing.path == input.path)
        {
            *existing = input;
        } else {
            self.inputs.push(input);
        }
    }
}

/// A path inside a folder; `.` and `..` of the name are kept as written.
fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() || name.starts_with('/') {
        name.to_owned()
    } else {
        format!("{}/{name}", folder.trim_end_matches('/'))
    }
}

/// Where a task reads a location from: a local path, or the URL.
fn source_of(location: &str) -> String {
    match files::local_path(location) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => location.to_owned(),
    }
}

/// Sets `path`, `dirname`, `basename`, `nameroot` and `nameext` of every staged file object
/// to its place inside the container, as cwltool's `check_adjust` does.
pub(super) fn adjust(value: &mut Value, paths: &HashMap<String, String>) {
    files::visit_files(value, &mut |file| {
        let location = file
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(path) = paths.get(location) else {
            return;
        };
        let path = path.clone();
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", &path));
        file.insert("dirname".into(), Value::string(folder));
        file.insert("basename".into(), Value::string(name));
        if file.get("class").and_then(Value::as_str) == Some("File") {
            let (root, extension) = files::split_extension(name);
            file.insert("nameroot".into(), Value::string(root));
            file.insert("nameext".into(), Value::string(extension));
        }
        file.insert("path".into(), Value::String(path));
    });
}
