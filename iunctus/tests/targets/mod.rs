//! The Nextflow workflows of the test workflows, which the test-workflows tests share: each by
//! its name, its target (a folder or a main script) and the folder of its project.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

/// A workflow by its name, its target and the folder of its project.
pub type Target = (String, PathBuf, PathBuf);

/// The nf-core pipelines, by name, in the `pipelines` folder of the test workflows.
pub fn nf_core(test_workflows: &Path) -> Vec<Target> {
    let folder = test_workflows.join("pipelines");
    let mut pipelines: Vec<String> = std::fs::read_dir(&folder)
        .expect("reads the test workflows")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    pipelines.sort();
    pipelines
        .into_iter()
        .map(|pipeline| {
            let project = folder.join(&pipeline);
            (pipeline, project.clone(), project)
        })
        .collect()
}

/// The Nextflow workflows of WorkflowHub (decision 4), by `workflowhub/<id>`: each runs its
/// folder as a project, with `manifest.mainScript` or `main.nf`, or else the main script that
/// its RO-Crate names.
pub fn workflowhub(test_workflows: &Path) -> Vec<Target> {
    let mut ids: Vec<String> = std::fs::read_dir(test_workflows)
        .expect("reads the test workflows")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !name.starts_with('.'))
        .collect();
    ids.sort_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX));
    ids.into_iter()
        .map(|id| {
            let folder = test_workflows.join(&id);
            let config = std::fs::read_to_string(folder.join("nextflow.config"));
            let project = folder.join("main.nf").exists()
                || config.is_ok_and(|text| text.contains("mainScript"));
            let target = match main_entity(&folder) {
                Some(main) if !project && main.ends_with(".nf") => folder.join(main),
                _ => folder.clone(),
            };
            (format!("workflowhub/{id}"), target, folder)
        })
        .collect()
}

/// The main script that the RO-Crate of a WorkflowHub entry names, relative to its folder.
fn main_entity(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(folder.join("ro-crate-metadata.json")).ok()?;
    let crate_: serde_json::Value = serde_json::from_str(&text).ok()?;
    let graph = crate_.get("@graph")?.as_array()?;
    let root = graph
        .iter()
        .find(|entity| entity.get("@id").and_then(|id| id.as_str()) == Some("./"))?;
    let main = root.get("mainEntity")?.get("@id")?.as_str()?;
    Some(main.trim_start_matches("./").to_owned())
}
