//! Selects container identity, permissions and output ownership for task execution.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use task_model::RunAs;

/// The option of the engines that keeps a container from gaining privileges.
pub const NO_NEW_PRIVILEGES: &str = "no-new-privileges";

/// The capability that lets root give files to another user.
pub const CHOWN: &str = "CHOWN";

/// The capabilities of the container that hands files back: `CHOWN`, and `DAC_READ_SEARCH`
/// to walk folders that only their owner may read.
pub const HAND_BACK_CAPABILITIES: [&str; 2] = [CHOWN, "DAC_READ_SEARCH"];

/// How a container of a task runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    /// The task's `run_as`: when set, the container runs as Kubernetes runs it, starting in
    /// the task's `workdir` or the working directory of its image, while the work directory
    /// stays in the scratch directory.
    pub run_as: Option<RunAs>,
    /// The user of the container as the engine takes it, like `1000:100`; `None` for the
    /// user of the image.
    pub user: Option<String>,
    /// The capabilities that the container keeps; it drops all others.
    pub capabilities: Vec<&'static str>,
    /// The ids that the user who runs iunctus has in the container, when the container may
    /// run as another user; what the task makes goes back to them.
    pub hand_back: Option<(u32, u32)>,
}

impl Access {
    /// How a container runs for a task with `run_as`, in an image whose config names the user
    /// `image_user` (like `1000:100`, `nobody` or empty for root).
    pub fn of(
        owner: (u32, u32),
        rootless: bool,
        run_as: Option<RunAs>,
        image_user: &str,
    ) -> Access {
        let inside = if rootless { (0, 0) } else { owner };
        let Some(given) = run_as else {
            return Access {
                user: Some(format!("{}:{}", inside.0, inside.1)),
                ..Access::default()
            };
        };
        let name = image_user.split(':').next().unwrap_or_default();
        let user = match (given.user, given.group) {
            (None, None) => None,
            (Some(user), None) => Some(user.to_string()),
            (Some(user), Some(group)) => Some(format!("{user}:{group}")),
            (None, Some(group)) if name.is_empty() => Some(format!("0:{group}")),
            (None, Some(group)) => Some(format!("{name}:{group}")),
        };
        let id = given.user.or(match name {
            "" | "root" => Some(0),
            name => name.parse().ok(),
        });
        let (capabilities, hand_back) = match id {
            Some(id) if id == inside.0 => (Vec::new(), None),
            Some(0) => (vec![CHOWN], Some(inside)),
            _ => (Vec::new(), Some(inside)),
        };
        Access {
            run_as,
            user,
            capabilities,
            hand_back,
        }
    }

    /// The ids that the wrapper gives what the task made, like `1000:100`; empty when the
    /// container runs as the user who runs iunctus.
    pub fn owner(&self) -> String {
        match self.hand_back {
            Some((user, group)) => format!("{user}:{group}"),
            None => String::new(),
        }
    }
}

/// Opens the scratch directory `root` of a task to every user, for a container that may run as
/// another user: its folders get mode 0777 and its files may be written by all.
pub fn open(root: &Path) -> std::io::Result<()> {
    if let Some(scratch) = root.parent() {
        std::fs::set_permissions(scratch, std::fs::Permissions::from_mode(0o700))?;
    }
    open_tree(root)
}

fn open_tree(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))?;
        for entry in std::fs::read_dir(path)? {
            open_tree(&entry?.path())?;
        }
    } else if metadata.is_file() {
        let mode = metadata.permissions().mode() | 0o666;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Whether anything in `root` belongs to another user than `user`, or cannot be read to tell.
pub fn foreign(root: &Path, user: u32) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(root) else {
        return false;
    };
    if metadata.uid() != user {
        return true;
    }
    if !metadata.is_dir() {
        return false;
    }
    match std::fs::read_dir(root) {
        Ok(entries) => (entries.flatten()).any(|entry| foreign(&entry.path(), user)),
        Err(_) => true,
    }
}

/// The command that hands a scratch directory back to `owner`, run in the task's image.
pub fn hand_back_command(root: &Path, owner: (u32, u32)) -> Vec<String> {
    vec![
        "chown".to_owned(),
        "-hR".to_owned(),
        format!("{}:{}", owner.0, owner.1),
        root.to_string_lossy().into_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_as(user: Option<u32>, group: Option<u32>) -> Option<RunAs> {
        Some(RunAs { user, group })
    }

    #[test]
    fn preserves_task_identity() {
        let access = Access::of((1000, 100), false, None, "nobody");
        assert_eq!(access.user.as_deref(), Some("1000:100"));
        assert!(access.capabilities.is_empty() && access.hand_back.is_none());
        // A rootless engine maps root of the container to the user.
        let access = Access::of((1000, 100), true, None, "nobody");
        assert_eq!(access.user.as_deref(), Some("0:0"));
    }

    #[test]
    fn returns_image_outputs() {
        // Root of a rootful engine keeps CHOWN to hand the files back.
        let access = Access::of((1000, 100), false, run_as(None, None), "");
        assert_eq!(access.user, None);
        assert_eq!(access.capabilities, [CHOWN]);
        assert_eq!(access.hand_back, Some((1000, 100)));
        assert_eq!(access.owner(), "1000:100");
        // Root of a rootless engine is the user.
        let access = Access::of((1000, 100), true, run_as(None, None), "root");
        assert!(access.capabilities.is_empty() && access.hand_back.is_none());
        // Another user needs a hand back, without capabilities.
        let access = Access::of((1000, 100), true, run_as(None, None), "nobody");
        assert!(access.capabilities.is_empty());
        assert_eq!(access.hand_back, Some((0, 0)));
        // The user and group of the template count.
        let access = Access::of((1000, 100), false, run_as(Some(1000), Some(5)), "");
        assert_eq!(access.user.as_deref(), Some("1000:5"));
        assert_eq!(access.hand_back, None);
        let access = Access::of((1000, 100), false, run_as(None, Some(5)), "curl:curl");
        assert_eq!(access.user.as_deref(), Some("curl:5"));
        let access = Access::of((1000, 100), false, run_as(None, Some(5)), "");
        assert_eq!(access.user.as_deref(), Some("0:5"));
    }

    #[test]
    fn permits_folder_writes() {
        let folder = tempfile::tempdir().expect("a folder");
        let root = folder.path().join("task");
        std::fs::create_dir_all(root.join("work")).expect("folders");
        std::fs::write(root.join("work/a.txt"), "a").expect("a file");
        std::os::unix::fs::symlink("/nowhere", root.join("work/link")).expect("a link");
        open(&root).expect("opens");
        let mode = |path: &Path| std::fs::metadata(path).expect("there").permissions().mode();
        assert_eq!(mode(&root.join("work")) & 0o777, 0o777);
        assert_eq!(mode(&root.join("work/a.txt")) & 0o777, 0o666);
        assert_eq!(mode(folder.path()) & 0o777, 0o700);
        let user = std::fs::metadata(&root).expect("there").uid();
        assert!(!foreign(&root, user));
        assert!(foreign(&root, user + 1));
    }
}
