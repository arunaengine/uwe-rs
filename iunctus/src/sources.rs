//! The sources of a run: the files of its workflow and the files that its command line names,
//! like config files, params files and CWL job files, each by digest. A real run keeps their
//! contents in its store, so that a repeat of the run has the same workflow and the same config
//! years later, also when the folder of the workflow has changed since.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use content_store::Store;
use run_database::{Source, SourceKind};
use task_model::{Tree, TreeEntry};

use crate::arguments::Arguments;
use crate::{detect, revision, tracked};

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

/// Keeps the contents of the sources in a store. The files of the workflow go in as one tree,
/// so that a later run of the same files finds the tree and stores nothing.
pub async fn keep(store: &Store, sources: &Sources) -> Result<(), String> {
    let failed = |error: content_store::StoreError| error.to_string();
    let workflow: Vec<&Source> = (sources.files.iter())
        .filter(|source| source.kind == SourceKind::Workflow)
        .collect();
    let tree = tree(&workflow).map_err(|error| error.to_string())?;
    let digest = tree.digest().map_err(|error| error.to_string())?;
    let stored = store.has_tree(&digest).await.map_err(failed)?;
    for source in &sources.files {
        let path = match source.kind {
            SourceKind::Workflow if stored => continue,
            SourceKind::Workflow => sources.folder.join(&source.path),
            SourceKind::Named => PathBuf::from(&source.path),
        };
        let (kept, _) = store.put_file(&path).await.map_err(failed)?;
        // A file that changed since it was hashed is kept as it is now; the run notes the
        // digest it read, so a repeat finds out.
        if kept.to_string() != source.digest {
            return Err(format!("{} changed while the run started", path.display()));
        }
    }
    if !stored {
        store.put_tree(&tree).await.map_err(failed)?;
    }
    Ok(())
}

/// The files of the workflow as a tree of the store.
pub fn tree(workflow: &[&Source]) -> Result<Tree, task_model::SpecError> {
    let entries = workflow
        .iter()
        .map(|source| {
            Ok(TreeEntry::File {
                path: source.path.clone(),
                digest: source.digest.parse().map_err(|_| {
                    task_model::SpecError::Encoding(format!("digest {}", source.digest))
                })?,
                size: source.size,
                executable: false,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Tree { entries })
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
        let mut files: Vec<Found> = (paths.into_iter())
            .map(|relative| Found {
                path: root.join(&relative),
                relative,
            })
            .filter(|file| file.path.is_file())
            .collect();
        if let Some(main) = arguments
            .language
            .or_else(|| detect::language(target).ok())
            .and_then(|language| detect::main_file(target, language))
            && !files.iter().any(|file| file.path == main)
            && let Ok(relative) = main.strip_prefix(&root)
        {
            files.push(Found {
                relative: relative.to_string_lossy().into_owned(),
                path: main,
            });
            files.sort_by(|left, right| left.relative.cmp(&right.relative));
        }
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
    fn keeps_selected_entry() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let project = root.path().join("workflow");
        write(&project.join("a.nf"), "process MODULE {}");
        write(&project.join("main.nf"), "workflow {}");
        write(&project.join("unused.nf"), "process UNUSED {}");
        for args in [["init", "-q"].as_slice(), ["add", "a.nf"].as_slice()] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&project)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .status()
                    .expect("prepares test index")
                    .success()
            );
        }
        let runtime = workflow_graph::runtime().expect("a runtime");
        for target in [&project, &project.join("main.nf")] {
            let sources = runtime.block_on(collect(target, &Arguments::default()));
            assert_eq!(sources.folder, project);
            assert_eq!(
                sources
                    .files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>(),
                ["a.nf", "main.nf"]
            );
            let store = Store::local(&root.path().join("store")).expect("a store");
            runtime
                .block_on(keep(&store, &sources))
                .expect("keeps entrypoint");
            let main = sources
                .files
                .iter()
                .find(|file| file.path == "main.nf")
                .unwrap();
            assert_eq!(
                runtime
                    .block_on(store.get_content(&main.digest.parse().unwrap()))
                    .unwrap(),
                b"workflow {}".as_slice()
            );
        }
    }

    #[test]
    fn excludes_run_folders() {
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
        // The store keeps the files once.
        let store = Store::local(&root.path().join("store")).expect("a store");
        runtime.block_on(keep(&store, &sources)).expect("keeps");
        let workflow: Vec<&Source> = sources.files[..2].iter().collect();
        let digest = tree(&workflow).expect("a tree").digest().expect("a digest");
        assert!(runtime.block_on(store.has_tree(&digest)).expect("looks"));
        runtime
            .block_on(keep(&store, &sources))
            .expect("keeps again");
    }
}
