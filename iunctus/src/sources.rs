//! The sources of a run: the files of its workflow and the files that its command line names,
//! like config files, params files and CWL job files, each by digest.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::{Source, SourceKind};

use crate::arguments::Arguments;
use crate::{revision, tracked};

/// The most files, and bytes in all, that a run records of the folder of its workflow. A larger
/// folder likely holds data, which the inputs of tasks record by digest anyway.
const MOST_FILES: usize = 5_000;
const MOST_BYTES: u64 = 256 << 20;

/// Folders that runs write into, which are no part of a workflow outside Git.
const RUN_FOLDERS: &[&str] = &["work", "results", "output", "outputs", "logs"];

/// The files of a workflow and of its command line.
#[derive(Debug, Default)]
pub struct Sources {
    /// The folder of the workflow: the work tree of its Git repository, or the folder that the
    /// target is, or that holds it.
    pub folder: PathBuf,
    pub files: Vec<Source>,
    /// Why the files of the folder are not recorded, when they are not.
    pub skipped: Option<String>,
}

/// A file with its place and its digest.
struct Found {
    path: PathBuf,
    relative: String,
}

/// Finds the sources of a run of `target` with `arguments`, and hashes them.
pub async fn collect(target: &Path, arguments: &Arguments) -> Sources {
    let target = std::path::absolute(target).unwrap_or_else(|_| target.to_owned());
    let (folder, found, skipped) = workflow_files(&target, arguments);
    let mut files = Vec::new();
    for file in found {
        if let Ok((digest, size)) = content_store::hash_file(&file.path).await {
            files.push(Source {
                kind: SourceKind::Workflow,
                path: file.relative,
                digest: digest.to_string(),
                size,
            });
        }
    }
    let named = (arguments.configs.iter()).chain(arguments.inputs.as_ref());
    for path in named {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        if let Ok((digest, size)) = content_store::hash_file(&path).await {
            files.push(Source {
                kind: SourceKind::Named,
                path: path.to_string_lossy().into_owned(),
                digest: digest.to_string(),
                size,
            });
        }
    }
    Sources {
        folder,
        files,
        skipped,
    }
}

/// The folder of the workflow and its files: the files that Git tracks in its repository, else
/// the files of its folder. A folder that holds more than a workflow is left out.
fn workflow_files(target: &Path, arguments: &Arguments) -> (PathBuf, Vec<Found>, Option<String>) {
    let own = match target.is_dir() {
        true => target.to_owned(),
        false => target.parent().unwrap_or(Path::new("/")).to_owned(),
    };
    if let Some((root, git)) = revision::repository(target)
        && let Some(paths) = tracked::tracked(&git)
    {
        let files: Vec<Found> = (paths.into_iter())
            .map(|relative| Found {
                path: root.join(&relative),
                relative,
            })
            .filter(|file| file.path.is_file())
            .collect();
        // A large repository, like one of many workflows, gives the files of the folder of
        // the workflow.
        return match limited(&files) {
            None => (root, files, None),
            Some(_) => {
                let inside: Vec<Found> = (files.into_iter())
                    .filter(|file| file.path.starts_with(&own))
                    .filter_map(|file| {
                        let relative = file.path.strip_prefix(&own).ok()?;
                        let relative = relative.to_string_lossy().into_owned();
                        Some(Found { relative, ..file })
                    })
                    .collect();
                let skipped = limited(&inside);
                match skipped {
                    None => (own, inside, None),
                    Some(reason) => (own, Vec::new(), Some(reason)),
                }
            }
        };
    }
    let output = arguments
        .output_dir
        .as_ref()
        .and_then(|folder| std::path::absolute(folder).ok());
    let mut files = Vec::new();
    let complete = walk(&own, &own, output.as_deref(), &mut files);
    match complete.then(|| limited(&files)).flatten() {
        None if complete => (own, files, None),
        reason => {
            let reason = reason.unwrap_or_else(|| too_many(&own));
            (own, Vec::new(), Some(reason))
        }
    }
}

/// Why a list of files is too large to record, when it is.
fn limited(files: &[Found]) -> Option<String> {
    let bytes: u64 = (files.iter())
        .filter_map(|file| std::fs::metadata(&file.path).ok())
        .map(|found| found.len())
        .sum();
    match files.len() > MOST_FILES || bytes > MOST_BYTES {
        true => Some(format!(
            "the folder of the workflow holds {} files of {} MB, more than a workflow ({MOST_FILES} \
             files or {} MB)",
            files.len(),
            bytes >> 20,
            MOST_BYTES >> 20
        )),
        false => None,
    }
}

fn too_many(folder: &Path) -> String {
    format!(
        "{} holds more than {MOST_FILES} files, more than a workflow",
        folder.display()
    )
}

/// The files below `folder`, without hidden files and folders, the folders that runs write
/// into, and links. False when there are more than [`MOST_FILES`].
fn walk(root: &Path, folder: &Path, output: Option<&Path>, files: &mut Vec<Found>) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return true;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if name.starts_with('.') || kind.is_symlink() || Some(path.as_path()) == output {
            continue;
        }
        if kind.is_dir() {
            if folder == root && RUN_FOLDERS.contains(&name.as_str()) {
                continue;
            }
            if !walk(root, &path, output, files) {
                return false;
            }
        } else if kind.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().into_owned();
            files.push(Found { path, relative });
            if files.len() > MOST_FILES {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
        std::fs::write(path, text).expect("writes");
    }

    #[test]
    fn a_folder_outside_git_gives_its_files_without_the_folders_of_runs() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let folder = root.path().join("pipeline");
        write(&folder.join("main.nf"), "workflow {}");
        write(&folder.join("modules/a.nf"), "process A {}");
        write(&folder.join("work/ab/cd/.command.sh"), "x");
        write(&folder.join("results/out.txt"), "x");
        write(&folder.join(".nextflow/history"), "x");
        write(&folder.join("custom-out/x.txt"), "x");
        write(&root.path().join("params.yaml"), "a: 1\n");
        let arguments = Arguments {
            configs: vec![root.path().join("params.yaml")],
            output_dir: Some(folder.join("custom-out")),
            ..Arguments::default()
        };
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let sources = runtime.block_on(collect(&folder.join("main.nf"), &arguments));
        assert_eq!(sources.folder, folder);
        assert_eq!(sources.skipped, None);
        let paths: Vec<(SourceKind, &str)> = (sources.files.iter())
            .map(|source| (source.kind, source.path.as_str()))
            .collect();
        let params = root.path().join("params.yaml");
        assert_eq!(
            paths,
            [
                (SourceKind::Workflow, "main.nf"),
                (SourceKind::Workflow, "modules/a.nf"),
                (SourceKind::Named, params.to_str().expect("UTF-8")),
            ]
        );
        assert!(sources.files[0].digest.starts_with("sha256:"));
    }
}
