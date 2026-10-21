//! Stores what a finished task made: each output under the attempt and by its content, and the
//! captured streams. A local store takes the files over from the scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use content_store::{Store, StoredOutput, layout};
use executor::manifest::Collected;
use executor::{Finished, Staging};
use task_model::{Digest, Tree, TreeEntry};

use crate::runner::at;
use crate::{ContainerResult, DigestCache, RunnerError, StoredFile, TaskResult};

/// Outputs from this size on get their digests kept in the cache of digests; smaller files are
/// read again faster.
const KEPT_FROM: u64 = 1 << 20;

/// Stores the outputs of a finished attempt of `task` in the run `run`, and its captured
/// streams by their content.
pub(crate) async fn store_outputs(
    store: &Store,
    run: &str,
    finished: &Finished,
    task: &str,
    attempt: u32,
    digests: Option<&DigestCache>,
    staging: &Staging,
) -> Result<TaskResult, RunnerError> {
    let mut scratch = Scratch::of(&finished.root, staging)?;
    let mut directories = HashMap::new();
    // Resolve and copy links before moving any file that another output may link to.
    for collected in &finished.files {
        layout::output(run, task, attempt, &collected.path)?;
        let local = finished.local_path(&collected.path);
        scratch.checked(&local).await?;
        if collected.directory {
            let found = walk(&local, Reach::Anywhere).await?;
            for item in &found {
                if let Found::File { path, .. } = item {
                    scratch.prepare(path).await?;
                }
            }
            directories.insert(local, found);
        } else {
            scratch.prepare(&local).await?;
        }
    }
    let place = Place {
        run,
        task,
        attempt,
        scratch,
        digests,
    };
    let mut order: Vec<&Collected> = finished.files.iter().collect();
    order.sort_by_key(|collected| (!collected.directory, collected.path.len()));
    // The directories stored so far, by their paths, with their trees.
    let mut trees: Vec<(&str, Tree)> = Vec::new();
    let mut stored: Vec<(&str, StoredFile)> = Vec::new();
    for collected in order {
        let path = collected.path.as_str();
        let location = layout::output(run, task, attempt, path)?;
        let (digest, size) = if collected.directory {
            let tree = match trees
                .iter()
                .find_map(|(base, tree)| within(tree, base, path))
            {
                Some(entries) => Tree { entries },
                None => {
                    let local = finished.local_path(path);
                    let found = directories.remove(&local).unwrap_or_default();
                    store_directory(store, &place, found, path).await?
                }
            };
            let size = tree.size();
            let digest = store.put_tree(&tree).await?;
            store.put_directory(&location, &digest).await?;
            trees.push((path, tree));
            (digest, size)
        } else if let Some(file) = trees
            .iter()
            .find_map(|(base, tree)| file_in(tree, base, path))
        {
            file
        } else {
            let local = place.scratch.prepared(&finished.local_path(path))?;
            let output = store.adopt_output(run, task, attempt, path, &local).await?;
            place.remember(store, &output).await;
            (output.digest, output.size)
        };
        let file = StoredFile {
            path: path.to_owned(),
            digest,
            size,
            location,
            directory: collected.directory,
        };
        stored.push((path, file));
    }
    // The outputs keep the order of the manifest.
    let mut files = Vec::new();
    for collected in &finished.files {
        if let Some(index) = stored.iter().position(|(path, _)| *path == collected.path) {
            files.push(stored.swap_remove(index).1);
        }
    }
    let log = async |file: PathBuf| -> Result<Option<Digest>, RunnerError> {
        if !tokio::fs::try_exists(&file).await.map_err(at(&file))? {
            return Ok(None);
        }
        let file = place.scratch.checked(&file).await?;
        Ok(Some(store.put_file(&file).await?.0))
    };
    let times = finished.tool_times();
    Ok(TaskResult {
        exit_code: finished.exit_code,
        files,
        stdout: log(finished.stdout()).await?,
        stderr: log(finished.stderr()).await?,
        reused: false,
        usage: finished.usage,
        started: times.map(|(started, _)| started),
        ended: times.map(|(_, ended)| ended),
        check: None,
        containers: Vec::new(),
        address: finished.address.clone(),
    })
}

/// Stores the captured streams of the containers of a task that runs several by their
/// content, with how each ended. The streams of the main container are the task's, which are
/// stored already as `main`.
pub(crate) async fn store_containers(
    store: &Store,
    spec: &task_model::TaskSpec,
    finished: &Finished,
    main: (&Option<Digest>, &Option<Digest>),
) -> Result<Vec<ContainerResult>, RunnerError> {
    let control = finished.root.join(executor::CONTROL);
    let scratch = Scratch::of(&finished.root, &Staging::default())?;
    let mut results = Vec::new();
    for (position, ended) in executor::containers::ended(spec, &control)
        .into_iter()
        .enumerate()
    {
        if position == 0 {
            results.push(ContainerResult {
                name: ended.name,
                exit_code: ended.exit_code,
                stdout: main.0.clone(),
                stderr: main.1.clone(),
            });
            continue;
        }
        let log = async |file: Option<PathBuf>| -> Result<Option<Digest>, RunnerError> {
            match file {
                Some(file) => {
                    let file = scratch.checked(&file).await?;
                    Ok(Some(store.put_file(&file).await?.0))
                }
                None => Ok(None),
            }
        };
        results.push(ContainerResult {
            name: ended.name,
            exit_code: ended.exit_code,
            stdout: log(ended.stdout).await?,
            stderr: log(ended.stderr).await?,
        });
    }
    Ok(results)
}

/// The files and empty directories of a stored directory at `base` that lie in a directory
/// output at `path` inside it, relative to that output; `None` when the path is not inside.
fn within(tree: &Tree, base: &str, path: &str) -> Option<Vec<TreeEntry>> {
    let relative = below(base, path)?;
    let prefix = format!("{relative}/");
    let entries = tree.entries.iter().filter_map(|entry| match entry {
        TreeEntry::File {
            path,
            digest,
            size,
            executable,
        } => Some(TreeEntry::File {
            path: path.strip_prefix(&prefix)?.to_owned(),
            digest: digest.clone(),
            size: *size,
            executable: *executable,
        }),
        TreeEntry::EmptyDirectory { path } => Some(TreeEntry::EmptyDirectory {
            path: path.strip_prefix(&prefix)?.to_owned(),
        }),
        TreeEntry::Symlink { path, target } => Some(TreeEntry::Symlink {
            path: path.strip_prefix(&prefix)?.to_owned(),
            target: target.clone(),
        }),
    });
    Some(entries.collect())
}

/// The digest and size of a file output at `path` that a stored directory at `base` holds.
fn file_in(tree: &Tree, base: &str, path: &str) -> Option<(Digest, u64)> {
    let relative = below(base, path)?;
    tree.entries.iter().find_map(|entry| match entry {
        TreeEntry::File {
            path, digest, size, ..
        } if path == relative => Some((digest.clone(), *size)),
        _ => None,
    })
}

/// The path of an output relative to a directory output at `base`, when it is inside it. The
/// base `.` is the work directory, which holds every output with a relative path.
fn below<'a>(base: &str, path: &'a str) -> Option<&'a str> {
    match base {
        "." => Some(path).filter(|path| *path != "." && !path.starts_with('/')),
        _ => path.strip_prefix(base)?.strip_prefix('/'),
    }
}

/// The path of `relative` inside a directory output at `base`.
fn joined(base: &str, relative: &str) -> String {
    match base {
        "." => relative.to_owned(),
        _ => format!("{base}/{relative}"),
    }
}

/// The attempt that outputs belong to.
struct Place<'a> {
    run: &'a str,
    task: &'a str,
    attempt: u32,
    scratch: Scratch,
    digests: Option<&'a DigestCache>,
}

impl Place<'_> {
    /// Keeps the digest of a large output that a local store took over.
    async fn remember(&self, store: &Store, output: &StoredOutput) {
        let path = store.local_path(&output.location);
        if let (Some(digests), Some(path)) = (self.digests, path)
            && output.size >= KEPT_FROM
        {
            digests.keep_stored(&path, &output.digest).await;
        }
    }
}

/// The scratch directory of an attempt, whose files the store may take over.
struct Scratch {
    /// The directory, with its links resolved.
    root: PathBuf,
    /// Where files from outside it are copied first, and how many are there.
    copies: PathBuf,
    copied: AtomicUsize,
    allowed: Vec<(PathBuf, bool)>,
    prepared: HashMap<PathBuf, PathBuf>,
}

impl Scratch {
    fn of(root: &Path, staging: &Staging) -> Result<Scratch, RunnerError> {
        let allowed = staging
            .inputs
            .iter()
            .map(|input| {
                std::fs::canonicalize(&input.source)
                    .map(|path| (path, input.directory))
                    .map_err(at(&input.source))
            })
            .collect::<Result<_, _>>()?;
        Ok(Scratch {
            root: std::fs::canonicalize(root).map_err(at(root))?,
            copies: root.join("copies"),
            copied: AtomicUsize::new(0),
            allowed,
            prepared: HashMap::new(),
        })
    }

    async fn checked(&self, file: &Path) -> Result<PathBuf, RunnerError> {
        let resolved = tokio::fs::canonicalize(file).await.map_err(at(file))?;
        if resolved.starts_with(&self.root)
            || self.allowed.iter().any(|(path, directory)| {
                resolved == *path || (*directory && resolved.starts_with(path))
            })
        {
            return Ok(resolved);
        }
        Err(RunnerError::File {
            path: file.to_owned(),
            error: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the output leaves its scratch directory and staged inputs",
            ),
        })
    }

    async fn prepare(&mut self, file: &Path) -> Result<(), RunnerError> {
        if !self.prepared.contains_key(file) {
            let own = self.own(file).await?;
            self.prepared.insert(file.to_owned(), own);
        }
        Ok(())
    }

    fn prepared(&self, file: &Path) -> Result<PathBuf, RunnerError> {
        self.prepared
            .get(file)
            .cloned()
            .ok_or_else(|| RunnerError::File {
                path: file.to_owned(),
                error: std::io::Error::other("the output was not prepared for publication"),
            })
    }

    /// A file that the store may take over: the file itself when it is in the scratch
    /// directory, else a copy there. A file that the task reached through a link, like the
    /// file of an input, must stay where it is.
    async fn own(&self, file: &Path) -> Result<PathBuf, RunnerError> {
        let resolved = self.checked(file).await?;
        if resolved.starts_with(&self.root) && resolved == file {
            return Ok(file.to_owned());
        }
        (tokio::fs::create_dir_all(&self.copies).await).map_err(at(&self.copies))?;
        let count = self.copied.fetch_add(1, Ordering::Relaxed);
        let copy = self.copies.join(count.to_string());
        tokio::fs::copy(&resolved, &copy).await.map_err(at(file))?;
        Ok(copy)
    }
}

/// Stores the files of a directory output at `base`, under the output and by their content,
/// and the empty directories in a local store. The result is the tree of the directory.
async fn store_directory(
    store: &Store,
    place: &Place<'_>,
    found: Vec<Found>,
    base: &str,
) -> Result<Tree, RunnerError> {
    let Place {
        run, task, attempt, ..
    } = *place;
    if let Some(path) = store.local_path(&layout::output(run, task, attempt, base)?) {
        tokio::fs::create_dir_all(&path).await.map_err(at(&path))?;
    }
    let mut entries = Vec::new();
    for found in found {
        let inside = joined(base, found.relative());
        match found {
            Found::File {
                relative,
                path,
                executable,
            } => {
                let local = place.scratch.prepared(&path)?;
                let stored = store
                    .adopt_output(run, task, attempt, &inside, &local)
                    .await?;
                place.remember(store, &stored).await;
                entries.push(TreeEntry::File {
                    path: relative,
                    digest: stored.digest,
                    size: stored.size,
                    executable,
                });
            }
            Found::EmptyDirectory { relative } => {
                let location = layout::output(run, task, attempt, &inside)?;
                if let Some(path) = store.local_path(&location) {
                    tokio::fs::create_dir_all(&path).await.map_err(at(&path))?;
                }
                entries.push(TreeEntry::EmptyDirectory { path: relative });
            }
            Found::Symlink { relative, target } => {
                let location = layout::output(run, task, attempt, &inside)?;
                if let Some(path) = store.local_path(&location) {
                    tokio::fs::create_dir_all(path.parent().expect("a parent")).await?;
                    tokio::fs::symlink(&target, &path)
                        .await
                        .map_err(at(&path))?;
                }
                entries.push(TreeEntry::Symlink {
                    path: relative,
                    target,
                });
            }
        }
    }
    Ok(Tree { entries })
}

/// A file or an empty directory inside a directory.
pub(crate) enum Found {
    File {
        relative: String,
        path: PathBuf,
        executable: bool,
    },
    EmptyDirectory {
        relative: String,
    },
    Symlink {
        relative: String,
        target: String,
    },
}

impl Found {
    /// The path relative to the directory.
    fn relative(&self) -> &str {
        match self {
            Found::File { relative, .. }
            | Found::EmptyDirectory { relative }
            | Found::Symlink { relative, .. } => relative,
        }
    }
}

/// Which links to files a walk reads through: any for outputs, whose files [`Scratch`] checks
/// later, or only those that stay inside the directory, for inputs.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Reach {
    Anywhere,
    Inside,
}

/// The files and empty directories below a directory, by their paths relative to it, found on
/// a thread that may block. Links that stay inside it are kept without following them.
pub(crate) async fn walk(root: &Path, reach: Reach) -> Result<Vec<Found>, RunnerError> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || walk_now(&root, reach))
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

fn walk_now(root: &Path, reach: Reach) -> Result<Vec<Found>, RunnerError> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_owned(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let mut empty = true;
        for entry in std::fs::read_dir(&directory).map_err(at(&directory))? {
            let entry = entry?;
            empty = false;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| RunnerError::Name(entry.path()))?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type()?.is_symlink() {
                let file = std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file());
                let inside = resolved(root, Path::new(&relative)).is_some();
                if inside && !file {
                    let target = std::fs::read_link(entry.path())?
                        .into_os_string()
                        .into_string()
                        .map_err(|_| RunnerError::Name(entry.path()))?;
                    found.push(Found::Symlink { relative, target });
                    continue;
                }
                if !inside && !(file && reach == Reach::Anywhere) {
                    return Err(RunnerError::File {
                        path: entry.path(),
                        error: std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            "link target is absolute or outside the selected directory",
                        ),
                    });
                }
            }
            let metadata = std::fs::metadata(entry.path()).map_err(at(&entry.path()))?;
            if metadata.is_dir() {
                pending.push((entry.path(), relative));
            } else if metadata.is_file() {
                found.push(Found::File {
                    relative,
                    path: entry.path(),
                    executable: metadata.permissions().mode() & 0o111 != 0,
                });
            }
        }
        if empty && !prefix.is_empty() {
            found.push(Found::EmptyDirectory { relative: prefix });
        }
    }
    Ok(found)
}

/// Where a path below `root` leads with its links followed, relative to `root`: `None` when
/// it leaves `root`, is absolute or loops. Missing parts count by name.
pub fn resolved(root: &Path, relative: &Path) -> Option<PathBuf> {
    let parts = |path: &Path| -> Vec<PathBuf> {
        (path.components().rev())
            .map(|part| PathBuf::from(part.as_os_str()))
            .collect()
    };
    let mut pending = parts(relative);
    let (mut current, mut hops) = (PathBuf::new(), 0);
    while let Some(part) = pending.pop() {
        match part.components().next()? {
            std::path::Component::Normal(name) => {
                let next = current.join(name);
                let place = root.join(&next);
                if !std::fs::symlink_metadata(&place).is_ok_and(|meta| meta.is_symlink()) {
                    current = next;
                    continue;
                }
                hops += 1;
                if hops > 40 {
                    return None;
                }
                pending.extend(parts(&std::fs::read_link(&place).ok()?));
            }
            std::path::Component::ParentDir => {
                if !current.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finished attempt in `root/scratch/attempt` that collected these outputs.
    fn finished(root: &Path, files: &[(&str, bool)]) -> Finished {
        let scratch = root.join("scratch").join("attempt");
        std::fs::create_dir_all(scratch.join("work")).expect("makes the work directory");
        Finished {
            address: None,
            exit_code: Some(0),
            files: (files.iter())
                .map(|(path, directory)| Collected {
                    path: (*path).to_owned(),
                    directory: *directory,
                })
                .collect(),
            root: scratch,
            usage: None,
        }
    }

    fn store_all(root: &Path, finished: &Finished) -> TaskResult {
        let store = Store::local(&root.join("store")).expect("a store");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let staged = Staging {
            inputs: vec![executor::Staged {
                source: root.join("input"),
                directory: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let staged = if root.join("input").exists() {
            staged
        } else {
            Staging::default()
        };
        let stored = store_outputs(&store, "run", finished, "task", 1, None, &staged);
        runtime.block_on(stored).expect("stores the outputs")
    }

    #[test]
    fn shares_directory_outputs() {
        let root = tempfile::tempdir().expect("a temporary directory");
        // Snakemake's test_github_issue3687: `directory('dir1/D')` and `'dir1/D/file.txt'`.
        let finished = finished(root.path(), &[("dir1/D", true), ("dir1/D/file.txt", false)]);
        let folder = finished.root.join("work/dir1/D");
        std::fs::create_dir_all(&folder).expect("makes the folder");
        std::fs::write(folder.join("file.txt"), "x\n").expect("writes");
        let result = store_all(root.path(), &finished);
        let paths: Vec<&str> = result.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["dir1/D", "dir1/D/file.txt"]);
        let file = &result.files[1];
        assert_eq!(file.digest, Digest::sha256(b"x\n"));
        assert_eq!(file.location, "runs/run/task/1/work/dir1/D/file.txt");
        let stored = root.path().join("store").join(&file.location);
        assert_eq!(std::fs::read_to_string(stored).expect("reads"), "x\n");
        assert!(result.files[0].directory);
    }

    #[test]
    fn adopts_work_directory() {
        let root = tempfile::tempdir().expect("a temporary directory");
        // nf-core's EPANG_PLACE: `path("./.")` and `path "*.epa_info.log"`.
        let finished = finished(root.path(), &[(".", true), ("x.epa_info.log", false)]);
        let work = finished.root.join("work");
        std::fs::create_dir_all(work.join("sub")).expect("makes a folder");
        std::fs::write(work.join("x.epa_info.log"), "log\n").expect("writes");
        std::fs::write(work.join("sub/a.txt"), "a\n").expect("writes");
        let result = store_all(root.path(), &finished);
        assert_eq!(result.files[0].location, "runs/run/task/1/work");
        assert!(result.files[0].directory);
        assert_eq!(result.files[0].size, 6);
        assert_eq!(result.files[1].digest, Digest::sha256(b"log\n"));
        let stored = root.path().join("store/runs/run/task/1/work");
        assert_eq!(
            std::fs::read_to_string(stored.join("sub/a.txt")).expect("reads"),
            "a\n"
        );
        let log = stored.join("x.epa_info.log");
        assert_eq!(std::fs::read_to_string(log).expect("reads"), "log\n");
    }

    #[test]
    fn preserves_link_locations() {
        let root = tempfile::tempdir().expect("a temporary directory");
        // A directory output that is a link to an input, and a file output below a link.
        let input = root.path().join("input");
        std::fs::create_dir_all(&input).expect("makes the input");
        std::fs::write(input.join("a.txt"), "a\n").expect("writes");
        let finished = finished(root.path(), &[("linked", true), ("via/a.txt", false)]);
        let work = finished.root.join("work");
        std::os::unix::fs::symlink(&input, work.join("linked")).expect("links");
        std::os::unix::fs::symlink(&input, work.join("via")).expect("links");
        let result = store_all(root.path(), &finished);
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[1].digest, Digest::sha256(b"a\n"));
        let kept = std::fs::read_to_string(input.join("a.txt")).expect("the input is still there");
        assert_eq!(kept, "a\n");
        let stored = root.path().join("store/runs/run/task/1/work/linked/a.txt");
        assert_eq!(std::fs::read_to_string(stored).expect("reads"), "a\n");
    }

    #[test]
    fn preserves_output_links() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let finished = finished(root.path(), &[("a", false), ("longlink", false)]);
        let work = finished.root.join("work");
        std::fs::write(work.join("a"), "made\n").expect("writes");
        std::os::unix::fs::symlink("a", work.join("longlink")).expect("links");
        let result = store_all(root.path(), &finished);
        assert_eq!(result.files.len(), 2);
        for file in result.files {
            let path = root.path().join("store").join(file.location);
            assert_eq!(std::fs::read_to_string(path).expect("reads"), "made\n");
        }
    }

    #[test]
    fn preserves_directory_links() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let finished = finished(root.path(), &[("dir", true)]);
        let directory = finished.root.join("work/dir");
        std::fs::create_dir_all(&directory).expect("makes directory");
        std::fs::write(directory.join("a"), "made\n").expect("writes");
        std::os::unix::fs::symlink("a", directory.join("linked")).expect("links");
        let result = store_all(root.path(), &finished);
        let path = root.path().join("store").join(&result.files[0].location);
        assert_eq!(
            std::fs::read_to_string(path.join("linked")).expect("reads"),
            "made\n"
        );
    }

    #[test]
    fn rejects_unstaged_links() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let finished = finished(root.path(), &[("out", false)]);
        let sentinel = root.path().join("host-only");
        std::fs::write(&sentinel, "private\n").expect("writes");
        std::os::unix::fs::symlink(&sentinel, finished.root.join("work/out")).expect("links");
        let store = Store::local(&root.path().join("store")).expect("a store");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let staging = Staging::default();
        let result = runtime.block_on(store_outputs(
            &store, "run", &finished, "task", 1, None, &staging,
        ));
        assert!(
            result
                .expect_err("refuses host read")
                .to_string()
                .contains("leaves")
        );
        assert!(finished.root.join("work/out").exists());
        assert!(!root.path().join("store/runs/run").exists());
    }

    #[test]
    fn inputs_keep_inside() {
        let root = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(root.path().join("secret"), "host\n").expect("writes");
        let tree = root.path().join("tree");
        std::fs::create_dir(&tree).expect("makes the tree");
        std::fs::write(tree.join("data"), "data\n").expect("writes");
        std::os::unix::fs::symlink(".", tree.join("a")).expect("links");
        std::os::unix::fs::symlink("a/a/data", tree.join("near")).expect("links");
        let found = walk_now(&tree, Reach::Inside).expect("walks inside links");
        let near = found
            .iter()
            .any(|item| matches!(item, Found::File { relative, .. } if relative == "near"));
        assert!(near);
        // `a/a/x` is `x`, which leads one folder up to a file of the host.
        std::os::unix::fs::symlink("../secret", tree.join("a/a/x")).expect("links");
        let error = walk_now(&tree, Reach::Inside)
            .err()
            .expect("refuses the host file");
        assert!(error.to_string().contains("outside"), "{error}");
        assert!(walk_now(&tree, Reach::Anywhere).is_ok());
    }
}
