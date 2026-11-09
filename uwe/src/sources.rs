//! Collects workflow sources and command-line files for run recording.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use content_store::Store;
use run_database::{Source, SourceKind};
use task_model::{Tree, TreeEntry};

use crate::arguments::Arguments;
use crate::{detect, repository, revision, tracked};

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

impl Sources {
    /// Leaves out the files of `paths`, like the files of the values of a compiled run, which
    /// no record or store keeps.
    pub fn leave_out(&mut self, paths: &[impl AsRef<Path>]) {
        let same = |path: &Path| paths.iter().any(|other| other.as_ref() == path);
        let folder = &self.folder;
        self.files.retain(|source| match source.kind {
            SourceKind::Workflow => !same(&folder.join(&source.path)),
            SourceKind::Named => !same(Path::new(&source.path)),
        });
    }

    /// Strict runs require a complete, nonempty source inventory.
    pub fn complete(&self) -> Result<(), String> {
        match &self.skipped {
            Some(reason) => Err(reason.clone()),
            None if !self
                .files
                .iter()
                .any(|source| source.kind == SourceKind::Workflow) =>
            {
                Err("the workflow has no recorded sources".into())
            }
            None => Ok(()),
        }
    }
}

/// A file with its place and its digest.
struct Found {
    path: PathBuf,
    relative: String,
}

/// Finds the sources of a run of `target` with `arguments`, and hashes them.
pub async fn collect(target: &Path, arguments: &Arguments) -> Sources {
    let remote = storage::address::scheme(&target.to_string_lossy()).is_some();
    let target = std::path::absolute(target).unwrap_or_else(|_| target.to_owned());
    let (folder, found, mut skipped) = if remote {
        (PathBuf::new(), Vec::new(), None)
    } else {
        workflow_files(&target, arguments)
    };
    let mut files = Vec::new();
    let secret = secret_store(&target, arguments);
    for file in found {
        if is_secret(&file.path, secret.as_deref()) {
            continue;
        }
        if let Ok((digest, size)) = content_store::hash_file(&file.path).await {
            files.push(Source {
                kind: SourceKind::Workflow,
                path: file.relative,
                digest: digest.to_string(),
                size,
                executable: std::fs::metadata(&file.path)
                    .is_ok_and(|metadata| metadata.mode() & 0o111 != 0),
            });
        } else {
            skipped = Some(format!("cannot record {}", file.path.display()));
        }
    }
    named(arguments, secret.as_deref(), &mut files, &mut skipped).await;
    Sources {
        folder,
        files,
        skipped,
    }
}

/// The sources of a run of a compiled document: the document alone as its workflow, and the
/// files of its command line. The folder of the document holds those of other runs too.
pub async fn compiled(document: &Path, arguments: &Arguments) -> Sources {
    let document = std::path::absolute(document).unwrap_or_else(|_| document.to_owned());
    let folder = document.parent().unwrap_or(Path::new("/")).to_owned();
    let (mut files, mut skipped) = (Vec::new(), None);
    match content_store::hash_file(&document).await {
        Ok((digest, size)) => files.push(Source {
            kind: SourceKind::Workflow,
            path: document
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            digest: digest.to_string(),
            size,
            executable: false,
        }),
        Err(_) => skipped = Some(format!("cannot record {}", document.display())),
    }
    named(arguments, None, &mut files, &mut skipped).await;
    Sources {
        folder,
        files,
        skipped,
    }
}

/// Adds the config and inputs files of the command line to `files`.
async fn named(
    arguments: &Arguments,
    secret: Option<&Path>,
    files: &mut Vec<Source>,
    skipped: &mut Option<String>,
) {
    let named = (arguments.configs.iter()).chain(arguments.inputs.as_ref());
    let secrets = crate::lower::secret_texts(&arguments.secrets);
    for path in named {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        if is_secret(&path, secret) {
            continue;
        }
        // A file that holds a secret value of the run stays out of the record and the store.
        let read = std::fs::read(&path);
        if read.is_ok_and(|bytes| crate::lower::holds_secret(&bytes, &secrets)) {
            continue;
        }
        if let Ok((digest, size)) = content_store::hash_file(&path).await {
            files.push(Source {
                kind: SourceKind::Named,
                path: path.to_string_lossy().into_owned(),
                digest: digest.to_string(),
                size,
                executable: std::fs::metadata(&path)
                    .is_ok_and(|metadata| metadata.mode() & 0o111 != 0),
            });
        } else {
            *skipped = Some(format!("cannot record {}", path.display()));
        }
    }
}

fn secret_store(target: &Path, arguments: &Arguments) -> Option<PathBuf> {
    if arguments.language.or_else(|| detect::language(target).ok())
        != Some(workflow_graph::Language::Nextflow)
    {
        return None;
    }
    struct Environment;
    impl nextflow_evaluator::interpreter::Host for Environment {
        fn print(&mut self, _: &str) {}
    }
    let path = nextflow_evaluator::secrets::path(&Environment);
    Some(
        path.canonicalize()
            .unwrap_or_else(|_| std::path::absolute(&path).unwrap_or(path)),
    )
}

fn is_secret(path: &Path, secret: Option<&Path>) -> bool {
    secret.is_some_and(|secret| {
        path.canonicalize().unwrap_or_else(|_| path.to_owned()) == secret
            || match (std::fs::metadata(path), std::fs::metadata(secret)) {
                (Ok(path), Ok(secret)) => path.dev() == secret.dev() && path.ino() == secret.ino(),
                _ => false,
            }
    })
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
                executable: source.executable,
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
    if let Some((root, _)) = repository::root(target) {
        let mut files = Vec::new();
        let complete = walk(&root, &root, None, &mut files);
        let skipped = limited(&files).or_else(|| (!complete).then(|| too_many(&root)));
        if skipped.is_some() {
            files.clear();
        }
        return (root, files, skipped);
    }
    if target.is_file()
        && arguments.target.as_ref().is_some_and(|path| {
            let text = path.to_string_lossy();
            text.starts_with("https://") || text.starts_with("http://")
        })
    {
        return (
            own,
            vec![Found {
                path: target.to_owned(),
                relative: target.file_name().unwrap().to_string_lossy().into_owned(),
            }],
            None,
        );
    }
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
        if (name.starts_with('.') && !root.join(repository::MARKER).is_file())
            || name == repository::MARKER
            || name == ".git"
            || kind.is_symlink()
            || Some(path.as_path()) == output
        {
            continue;
        }
        if kind.is_dir() {
            if folder == root
                && !root.join(repository::MARKER).is_file()
                && RUN_FOLDERS.contains(&name.as_str())
            {
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

    #[test]
    fn rejects_incomplete_capture() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("main.nf");
        std::fs::write(&target, "workflow {}").unwrap();
        let arguments = Arguments {
            configs: vec![root.path().join("missing.config")],
            ..Arguments::default()
        };
        let sources = workflow_graph::runtime()
            .unwrap()
            .block_on(collect(&target, &arguments));
        assert!(!sources.files.is_empty());
        assert!(sources.complete().unwrap_err().contains("missing.config"));
        assert!(Sources::default().complete().is_err());
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
        std::fs::write(path, text).expect("writes");
    }

    #[test]
    fn excludes_secret_aliases() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secrets.json");
        write(&secret, "[]");
        let alias = root.path().join("alias.json");
        std::os::unix::fs::symlink(&secret, &alias).unwrap();
        let linked = root.path().join("linked.json");
        std::fs::hard_link(&secret, &linked).unwrap();
        assert!(super::is_secret(&secret, Some(&secret)));
        assert!(super::is_secret(&alias, Some(&secret)));
        assert!(super::is_secret(&linked, Some(&secret)));
        assert!(!super::is_secret(
            &root.path().join("main.nf"),
            Some(&secret)
        ));
        assert!(!super::is_secret(&secret, None));
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
