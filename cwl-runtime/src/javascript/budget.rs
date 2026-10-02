//! Verifies that the exported worker is inside its enforced container budget.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

const MEMORY_LIMIT: u64 = 256 << 20;

fn marker_allowed(marker: Option<&str>, container: bool) -> Result<(), String> {
    if marker != Some("1") || !container {
        return Err("CWL JavaScript worker requires its marked Docker or Podman container".into());
    }
    Ok(())
}

fn mount_path(field: &str) -> PathBuf {
    PathBuf::from(
        field
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\"),
    )
}

fn current_group(groups: &str, mounts: &str) -> Result<(PathBuf, bool), String> {
    let memberships: Vec<_> = groups
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            Some((fields.next()?, fields.next()?, fields.next()?))
        })
        .collect();
    let (controllers, group) = memberships
        .iter()
        .find(|(_, controllers, _)| controllers.split(',').any(|name| name == "memory"))
        .or_else(|| {
            memberships
                .iter()
                .find(|(id, controllers, _)| *id == "0" && controllers.is_empty())
        })
        .map(|(_, controllers, group)| (*controllers, *group))
        .ok_or("CWL JavaScript worker cannot identify its memory cgroup")?;
    let unified = controllers.is_empty();
    for line in mounts.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = before.split_whitespace().collect();
        let filesystem: Vec<_> = after.split_whitespace().collect();
        if fields.len() < 5 || filesystem.len() < 3 {
            continue;
        }
        let matched = if unified {
            filesystem[0] == "cgroup2"
        } else {
            filesystem[0] == "cgroup" && filesystem[2].split(',').any(|name| name == "memory")
        };
        if !matched {
            continue;
        }
        let root = mount_path(fields[3]);
        let group = Path::new(group);
        let Ok(relative) = group.strip_prefix(&root) else {
            continue;
        };
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            continue;
        }
        return Ok((mount_path(fields[4]).join(relative), unified));
    }
    Err("CWL JavaScript worker cannot locate its enforced memory cgroup".into())
}

fn prove_limits(group: &Path, unified: bool) -> Result<(), String> {
    let read = |name| -> Result<u64, String> {
        std::fs::read_to_string(group.join(name))
            .map_err(|error| format!("CWL JavaScript worker cannot read {name}: {error}"))?
            .trim()
            .parse()
            .map_err(|_| format!("CWL JavaScript worker has unavailable or unlimited {name}"))
    };
    let memory = read(if unified {
        "memory.max"
    } else {
        "memory.limit_in_bytes"
    })?;
    if memory == 0 || memory > MEMORY_LIMIT {
        return Err("CWL JavaScript worker memory ceiling exceeds its 256 MiB budget".into());
    }
    let swap = read(if unified {
        "memory.swap.max"
    } else {
        "memory.memsw.limit_in_bytes"
    })?;
    if (unified && swap != 0) || (!unified && swap > memory) {
        return Err("CWL JavaScript worker requires an enforced zero-swap budget".into());
    }
    Ok(())
}

pub(crate) fn prove_worker_budget() -> Result<(), String> {
    marker_allowed(
        std::env::var("IUNCTUS_CWL_WORKER").ok().as_deref(),
        Path::new("/.dockerenv").is_file() || Path::new("/run/.containerenv").is_file(),
    )?;
    let groups = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| format!("CWL JavaScript worker cannot read cgroup membership: {error}"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("CWL JavaScript worker cannot read cgroup mounts: {error}"))?;
    let (group, unified) = current_group(&groups, &mounts)?;
    prove_limits(&group, unified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_worker_isolation() {
        assert!(marker_allowed(Some("1"), false).is_err());
        assert!(marker_allowed(None, true).is_err());
        assert!(marker_allowed(Some("1"), true).is_ok());
    }

    #[test]
    fn resolves_cgroup_membership() {
        assert_eq!(
            current_group(
                "0::/\n",
                "1 0 0:0 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n"
            ),
            Ok((PathBuf::from("/sys/fs/cgroup"), true))
        );
        assert_eq!(
            current_group(
                "4:memory:/docker/worker/nested\n",
                "1 0 0:0 /docker/worker /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n"
            ),
            Ok((PathBuf::from("/sys/fs/cgroup/memory/nested"), false))
        );
        assert!(
            current_group(
                "0::/../../other\n",
                "1 0 0:0 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n"
            )
            .is_err()
        );
    }

    #[test]
    fn requires_memory_bounds() {
        let group = tempfile::tempdir().expect("group");
        std::fs::write(group.path().join("memory.max"), MEMORY_LIMIT.to_string()).expect("memory");
        assert!(prove_limits(group.path(), true).is_err());
        for (memory, swap, accepted) in [
            (MEMORY_LIMIT.to_string(), "0", true),
            ("max".into(), "0", false),
            ((MEMORY_LIMIT + 1).to_string(), "0", false),
            (MEMORY_LIMIT.to_string(), "1", false),
        ] {
            std::fs::write(group.path().join("memory.max"), memory).expect("memory");
            std::fs::write(group.path().join("memory.swap.max"), swap).expect("swap");
            assert_eq!(prove_limits(group.path(), true).is_ok(), accepted);
        }
        std::fs::write(
            group.path().join("memory.limit_in_bytes"),
            MEMORY_LIMIT.to_string(),
        )
        .expect("memory");
        std::fs::write(
            group.path().join("memory.memsw.limit_in_bytes"),
            MEMORY_LIMIT.to_string(),
        )
        .expect("memsw");
        assert!(prove_limits(group.path(), false).is_ok());
        std::fs::write(
            group.path().join("memory.memsw.limit_in_bytes"),
            (MEMORY_LIMIT + 1).to_string(),
        )
        .expect("memsw");
        assert!(prove_limits(group.path(), false).is_err());
    }
}
