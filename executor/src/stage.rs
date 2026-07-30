//! Places the inputs of a task in its scratch directory before its container starts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::job::{CONTROL, MOUNTS, Staged, Staging, WORK};
use crate::process::Mount;
use crate::wrapper::STDIN;

/// Makes the scratch directory `root` of a task and places its inputs. The result is what the
/// container must mount besides the scratch directory.
///
/// A linked input is a symbolic link to its source, and the container sees the folder of the
/// source read-only at the same path. A copy is the task's own. A linked input at an absolute
/// path is mounted there read-only; a copy and a made directory at an absolute path are made
/// in `mounts/` and mounted there. An input that the task changes in place is mounted
/// writable at its path; the target of such a mount is relative to the work directory for a
/// relative path, and `.` is the work directory itself.
pub fn place(root: &Path, staging: &Staging) -> std::io::Result<Vec<Mount>> {
    let work = root.join(WORK);
    let control = root.join(CONTROL);
    if let Some(scratch) = root.parent() {
        std::fs::create_dir_all(scratch)?;
    }
    std::fs::create_dir(root)?;
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(&control)?;
    let place = |path: &str| match path.strip_prefix('/') {
        Some(inside) => root.join(MOUNTS).join(inside),
        None => work.join(path),
    };
    let mut shown = BTreeSet::new();
    let mut made = BTreeSet::new();
    let mut mounts = Vec::new();
    for directory in &staging.directories {
        std::fs::create_dir_all(place(directory))?;
        if directory.starts_with('/') {
            made.insert(PathBuf::from(directory.trim_end_matches('/')));
        }
    }
    for input in &staging.inputs {
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
        if input.copy {
            copy(input, &target)?;
            if path.starts_with('/') {
                made.insert(PathBuf::from(path));
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
    fn links_and_copies_find_their_places() {
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
    fn copied_directories_are_writable() {
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
    fn inputs_at_their_own_paths_share_the_mounts_of_shown_folders() {
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
    fn inputs_changed_in_place_are_mounted_writable_at_their_paths() {
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
    fn folders_inside_others_are_not_shown_twice() {
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
