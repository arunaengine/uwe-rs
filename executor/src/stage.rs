//! Places the inputs of a task in its scratch directory before its container starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::job::{CONTROL, MOUNTS, Staged, Staging, WORK};
use crate::process::Mount;
use crate::wrapper::STDIN;

/// Makes the scratch directory `root` of a task and places its inputs.
pub fn place(root: &Path, staging: &Staging) -> std::io::Result<Vec<Mount>> {
    for path in staging
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .chain(staging.directories.iter().map(String::as_str))
    {
        if path.is_empty() || path.contains('\0') || path.split('/').any(|part| part == "..") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "input path escapes its staging directory",
            ));
        }
    }
    let work = root.join(WORK);
    let control = root.join(CONTROL);
    if let Some(scratch) = root.parent() {
        std::fs::create_dir_all(scratch)?;
    }
    std::fs::create_dir(root)?;
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(&control)?;
    let place = |path: &str| match path.starts_with('/') {
        true => root.join(MOUNTS).join(path.trim_start_matches('/')),
        false => work.join(path),
    };
    let destinations: Vec<Option<PathBuf>> = staging
        .inputs
        .iter()
        .map(|input| {
            (!input.in_place && !(input.path.starts_with('/') && !input.copy)).then(|| {
                place(&input.path)
                    .components()
                    .filter(|part| *part != std::path::Component::CurDir)
                    .collect()
            })
        })
        .collect();
    let mut shown = BTreeSet::new();
    let mut made = BTreeSet::new();
    let mut mounts = Vec::new();
    for directory in &staging.directories {
        std::fs::create_dir_all(place(directory))?;
        if directory.starts_with('/') {
            made.insert(Path::new(directory).components().collect());
        }
    }
    for (index, input) in staging.inputs.iter().enumerate() {
        let path = input.path.trim_end_matches('/');
        if input.in_place {
            mounts.push(mount(input.source.clone(), path, false));
            continue;
        }
        if path.starts_with('/') && !input.copy {
            // An input at its own path is shown as the folders of linked inputs are, so that
            // the container gets one mount where both need the same place.
            if Path::new(path) == input.source {
                shown.insert(input.source.clone());
            } else {
                mounts.push(mount(input.source.clone(), path, true));
            }
            continue;
        }
        let target = place(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let overlaps = destinations[index].as_ref().is_some_and(|path| {
            destinations.iter().enumerate().any(|(other, destination)| {
                other != index
                    && destination.as_ref().is_some_and(|destination| {
                        path.starts_with(destination) || destination.starts_with(path)
                    })
            })
        });
        if input.copy || overlaps {
            copy(input, &target)?;
            if path.starts_with('/') {
                made.insert(Path::new(path).components().collect());
            }
        } else {
            std::os::unix::fs::symlink(&input.source, &target)?;
            shown.insert(shown_folder(input));
        }
    }
    for path in outermost(made) {
        let inside = path.to_string_lossy().into_owned();
        mounts.push(mount(place(&inside), &inside, false));
    }
    for folder in outermost(shown) {
        let target = folder.to_string_lossy().into_owned();
        mounts.push(mount(folder, &target, true));
    }
    if let Some(stdin) = &staging.stdin {
        std::fs::copy(stdin, control.join(STDIN))?;
    }
    Ok(mounts)
}

fn mount(source: PathBuf, target: &str, read_only: bool) -> Mount {
    Mount {
        source,
        target: target.to_owned(),
        read_only,
    }
}

/// The folder the container must see for a linked input: the directory itself, or the folder
/// of a file. A file right below a top folder like `/tmp` is shown alone, so that the container
/// keeps its own `/tmp`.
fn shown_folder(input: &Staged) -> PathBuf {
    if input.directory {
        return input.source.clone();
    }
    match input.source.parent() {
        Some(parent) if parent.components().count() > 2 => parent.to_owned(),
        _ => input.source.clone(),
    }
}

/// The folders that are not inside another one of them.
fn outermost(folders: BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    // In sorted order a folder comes right before the folders inside it.
    for folder in folders {
        if !kept.iter().any(|outer| folder.starts_with(outer)) {
            kept.push(folder);
        }
    }
    kept
}

/// Copies an input file or directory into place, executable when it is a program.
fn copy(input: &Staged, target: &Path) -> std::io::Result<()> {
    if input.directory {
        return copy_tree(&input.source, target);
    }
    std::fs::copy(&input.source, target)?;
    let mode = if input.executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
}

/// Copies a directory, which the task may change: its folders and files are writable, and
/// files keep whether they are programs.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_symlink() && !entry.path().is_file() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
            continue;
        }
        let metadata = std::fs::metadata(entry.path())?;
        if metadata.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
            let mode = match metadata.permissions().mode() & 0o111 {
                0 => 0o644,
                _ => 0o755,
            };
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str, source: &Path, copy: bool) -> Staged {
        Staged {
            path: path.into(),
            source: source.to_owned(),
            directory: false,
            copy,
            executable: false,
            ..Default::default()
        }
    }

    #[test]
    fn overlays_preserve_sources() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let victim = source.join("config");
        let replacement = folder.path().join("replacement");
        std::fs::write(&victim, "original").unwrap();
        std::fs::write(&replacement, "replacement").unwrap();
        let absolute = format!("/{}", victim.display());
        let root = folder.path().join("scratch/absolute");
        place(
            &root,
            &Staging {
                inputs: vec![input(&absolute, &replacement, true)],
                ..Staging::default()
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(root.join("mounts").join(absolute.trim_start_matches('/')))
                .unwrap(),
            "replacement"
        );
        for child in ["d/config", "./d/config"] {
            let mut directory = input("d", &source, false);
            directory.directory = true;
            let root = folder
                .path()
                .join(format!("scratch/overlap-{}", child.starts_with('.')));
            place(
                &root,
                &Staging {
                    inputs: vec![directory, input(child, &replacement, true)],
                    ..Staging::default()
                },
            )
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join("work/d/config")).unwrap(),
                "replacement"
            );
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        }
        let root = folder.path().join("scratch/reverse-overlay");
        let mut directory = input("./d", &source, false);
        directory.directory = true;
        place(
            &root,
            &Staging {
                inputs: vec![input("d/config", &replacement, false), directory],
                ..Staging::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("work/d/config")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(&replacement).unwrap(),
            "replacement"
        );
        let root = folder.path().join("scratch/alias-overlay");
        place(
            &root,
            &Staging {
                inputs: vec![
                    input("config", &victim, false),
                    input("./config", &replacement, false),
                ],
                ..Staging::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("work/config")).unwrap(),
            "replacement"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        let root = folder.path().join("scratch/absolute-overlay");
        let mut directory = input("/d", &source, true);
        directory.directory = true;
        let mounts = place(
            &root,
            &Staging {
                inputs: vec![directory, input("//d/config", &replacement, true)],
                ..Staging::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("mounts/d/config")).unwrap(),
            "replacement"
        );
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].target, "/d");
    }

    #[test]
    fn places_links_copies() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let data = folder.path().join("data");
        std::fs::create_dir(&data).expect("makes a folder");
        let source = data.join("a.txt");
        std::fs::write(&source, "a\n").expect("writes");
        let staging = Staging {
            inputs: vec![
                input("in/a.txt", &source, false),
                input("own.txt", &source, true),
                input("/abs/a.txt", &source, false),
                input("/copied/a.txt", &source, true),
            ],
            directories: vec!["/made/empty".into(), "/made/empty/deeper".into()],
            stdin: None,
        };
        let root = folder.path().join("scratch").join("task");
        let mounts = place(&root, &staging).expect("places");
        let link = std::fs::read_link(root.join("work/in/a.txt")).expect("a link");
        assert_eq!(link, source);
        assert!(
            !std::fs::symlink_metadata(root.join("work/own.txt"))
                .unwrap()
                .is_symlink()
        );
        let mounts: Vec<(String, bool)> = mounts
            .iter()
            .map(|mount| (mount.target.clone(), mount.read_only))
            .collect();
        let shown = data.to_string_lossy().into_owned();
        let expected = [
            ("/abs/a.txt".to_owned(), true),
            ("/copied/a.txt".to_owned(), false),
            ("/made/empty".to_owned(), false),
            (shown, true),
        ];
        assert_eq!(mounts, expected);
        assert!(root.join("mounts/made/empty/deeper").is_dir());
    }

    #[test]
    fn makes_directories_writable() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let source = folder.path().join("stored");
        std::fs::create_dir_all(source.join("c")).expect("makes the folders");
        std::fs::write(source.join("c/d"), "d\n").expect("writes");
        // A directory of the store, whose files and folders nobody may change.
        let read_only = std::fs::Permissions::from_mode(0o444);
        std::fs::set_permissions(source.join("c/d"), read_only).expect("sets the mode");
        let copied = Staged {
            path: "work_dir".into(),
            source: source.clone(),
            directory: true,
            copy: true,
            ..Staged::default()
        };
        let staging = Staging {
            inputs: vec![copied],
            ..Staging::default()
        };
        let root = folder.path().join("scratch").join("task");
        place(&root, &staging).expect("places");
        let mode = |path: &str| {
            let metadata = std::fs::metadata(root.join("work").join(path)).expect("exists");
            metadata.permissions().mode() & 0o777
        };
        assert_eq!(mode("work_dir/c/d"), 0o644);
        assert_eq!(mode("work_dir/c"), 0o755);
    }

    #[test]
    fn preserves_directory_links() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("stored");
        std::fs::create_dir_all(source.join("child")).unwrap();
        std::fs::write(source.join("child/file"), "original").unwrap();
        std::os::unix::fs::symlink("child", source.join("alias")).unwrap();
        std::os::unix::fs::symlink("..", source.join("child/parent")).unwrap();
        std::os::unix::fs::symlink("child/file", source.join("file-link")).unwrap();
        std::os::unix::fs::symlink("missing", source.join("dangling")).unwrap();
        std::os::unix::fs::symlink("b", source.join("a")).unwrap();
        std::os::unix::fs::symlink("a", source.join("b")).unwrap();
        let root = folder.path().join("scratch/task");
        let copied = Staged {
            path: "copied".into(),
            source: source.clone(),
            directory: true,
            copy: true,
            ..Staged::default()
        };
        place(
            &root,
            &Staging {
                inputs: vec![copied],
                ..Staging::default()
            },
        )
        .unwrap();
        let copied = root.join("work/copied");
        for (name, target) in [("dangling", "missing"), ("a", "b"), ("b", "a")] {
            assert_eq!(
                std::fs::read_link(copied.join(name)).unwrap(),
                Path::new(target)
            );
        }
        assert_eq!(
            std::fs::read_link(copied.join("alias")).unwrap(),
            Path::new("child")
        );
        assert_eq!(
            std::fs::read_link(copied.join("child/parent")).unwrap(),
            Path::new("..")
        );
        assert!(
            !std::fs::symlink_metadata(copied.join("file-link"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(copied.join("file-link")).unwrap(),
            "original"
        );
        std::fs::write(copied.join("alias/file"), "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("child/file")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(copied.join("child/parent/child/file")).unwrap(),
            "changed"
        );
    }

    #[test]
    fn shares_shown_mounts() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let scripts = folder.path().join("scripts");
        std::fs::create_dir(&scripts).expect("makes a folder");
        let script = scripts.join("a.py");
        std::fs::write(&script, "a\n").expect("writes");
        // A script folder at its own path, and a file of it linked into the work directory.
        let own = Staged {
            path: scripts.to_string_lossy().into_owned(),
            source: scripts.clone(),
            directory: true,
            ..Staged::default()
        };
        let staging = Staging {
            inputs: vec![own, input("scripts/a.py", &script, false)],
            ..Staging::default()
        };
        let root = folder.path().join("scratch").join("task");
        let mounts = place(&root, &staging).expect("places");
        let targets: Vec<&str> = mounts.iter().map(|mount| mount.target.as_str()).collect();
        assert_eq!(targets, [scripts.to_string_lossy()]);
    }

    #[test]
    fn mounts_changed_writable() {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let in_place = |path: &str| Staged {
            path: path.into(),
            source: folder.path().to_owned(),
            directory: true,
            in_place: true,
            ..Staged::default()
        };
        let staging = Staging {
            inputs: vec![in_place("."), in_place("data/"), in_place("/abs")],
            ..Staging::default()
        };
        let root = folder.path().join("scratch").join("task");
        let mounts = place(&root, &staging).expect("places");
        let mounts: Vec<(&str, bool)> = (mounts.iter())
            .map(|mount| (mount.target.as_str(), mount.read_only))
            .collect();
        assert_eq!(mounts, [(".", false), ("data", false), ("/abs", false)]);
        // Nothing is placed for them: the work directory stays empty.
        let placed = std::fs::read_dir(root.join("work")).expect("reads").count();
        assert_eq!(placed, 0);
    }

    #[test]
    fn omits_nested_mounts() {
        let folders: BTreeSet<PathBuf> = ["/a/b", "/a/b/c", "/a/bc", "/d"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let kept = outermost(folders);
        let kept: Vec<&str> = kept.iter().filter_map(|path| path.to_str()).collect();
        assert_eq!(kept, ["/a/b", "/a/bc", "/d"]);
        let file = |source: &str| input("x", Path::new(source), false);
        assert_eq!(
            shown_folder(&file("/tmp/x.txt")),
            PathBuf::from("/tmp/x.txt")
        );
        assert_eq!(shown_folder(&file("/tmp/a/x.txt")), PathBuf::from("/tmp/a"));
    }
}
