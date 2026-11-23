//! Finds the workflows below the folders of the studio and lists the configured presets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use run_page::WorkflowEntry;

/// How deep the search goes below each folder.
const DEPTH: usize = 4;

/// Folders that hold build outputs, run folders, caches or the modules of a pipeline.
const SKIPPED: [&str; 8] = [
    "target",
    "node_modules",
    "work",
    "results",
    "__pycache__",
    "venv",
    "modules",
    "subworkflows",
];

/// The workflows below `folders`, then the presets of the config file.
pub(super) fn workflows(folders: &[PathBuf], config: Option<&Path>) -> Vec<WorkflowEntry> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for folder in folders {
        let mut files = Vec::new();
        candidates(folder, 0, (&mut files, &mut seen));
        files.sort();
        found.extend(files.iter().filter_map(|path| workflow(path)));
    }
    let catalogue = match crate::catalogue::Catalogue::load(config) {
        Ok(catalogue) => catalogue,
        Err(failure) => {
            tracing::warn!("uwe: the presets cannot be read: {}", failure.message);
            return found;
        }
    };
    for (name, source, language) in catalogue.presets() {
        found.push(WorkflowEntry {
            id: format!("preset:{name}"),
            name: name.to_owned(),
            path: Some(source.to_string_lossy().into_owned()),
            language: language.map(|language| language.to_string()),
            preset: Some(name.to_owned()),
            description: Some(format!("The preset {name} of the config file.")),
        });
    }
    found
}

/// A file as a workflow of the list, when its name and contents say that it is one.
fn workflow(path: &Path) -> Option<WorkflowEntry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let lower = name.to_lowercase();
    let composed = crate::tools::composed::is_composed(path);
    let likely = composed
        || matches!(lower.as_str(), "main.nf" | "snakefile")
        || [".cwl", ".wdl", ".ga", ".wir.json", ".wir.yaml"]
            .iter()
            .any(|end| lower.ends_with(end))
        || (lower.ends_with(".yaml") || lower.ends_with(".yml")) && argo(path);
    if !likely {
        return None;
    }
    let (language, description) = match composed {
        true => {
            let text = std::fs::read_to_string(path).ok()?;
            let workflow = crate::tools::composed::parse(&text).ok();
            (
                "composed".to_owned(),
                workflow.and_then(|workflow| workflow.description),
            )
        }
        false => (crate::detect::language(path).ok()?.to_string(), None),
    };
    let shown = match lower.as_str() {
        "main.nf" | "snakefile" => path.parent()?.file_name()?.to_string_lossy().into_owned(),
        _ => name,
    };
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let path = path.to_string_lossy().into_owned();
    Some(WorkflowEntry {
        id: path.clone(),
        name: shown,
        path: Some(path),
        language: Some(language),
        preset: None,
        description,
    })
}

/// Whether a YAML file is an Argo workflow or workflow template.
fn argo(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.contains("argoproj.io/")
        && (text.lines()).any(|line| {
            matches!(
                line.trim(),
                "kind: Workflow" | "kind: WorkflowTemplate" | "kind: ClusterWorkflowTemplate"
            )
        })
}

/// The files below a folder that may be workflows. A linked folder is not followed, so that
/// the search stays inside the folder and ends, and no folder is searched twice.
fn candidates(
    folder: &Path,
    depth: usize,
    (found, seen): (&mut Vec<PathBuf>, &mut HashSet<PathBuf>),
) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    if !folder.canonicalize().is_ok_and(|real| seen.insert(real)) {
        return;
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if name.starts_with('.') || (kind.is_symlink() && path.is_dir()) {
            continue;
        }
        if kind.is_dir() {
            if depth < DEPTH && !SKIPPED.contains(&name.as_str()) {
                candidates(&path, depth + 1, (&mut *found, &mut *seen));
            }
            continue;
        }
        found.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_linked_folders() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(project.join("inner")).unwrap();
        std::fs::write(project.join("inner/main.cwl"), "cwlVersion: v1.2\n").unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("other.cwl"), "cwlVersion: v1.2\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("linked")).unwrap();
        std::os::unix::fs::symlink(&project, project.join("inner/loop")).unwrap();
        let (mut found, mut seen) = (Vec::new(), HashSet::new());
        candidates(&project, 0, (&mut found, &mut seen));
        assert_eq!(found, [project.join("inner/main.cwl")]);
        found.clear();
        candidates(&project, 0, (&mut found, &mut seen));
        assert!(found.is_empty(), "a folder is searched once");
    }
}
