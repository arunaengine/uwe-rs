//! The journal of the SQLite files of iunctus: WAL on a local file system, where readers go on
//! while a run writes, and the rollback journal on a file system that machines share, like NFS
//! or CephFS on a cluster. WAL keeps its index in memory that all its processes share, so they
//! must be on one machine: a run on a second node of a cluster cannot open the file ("database
//! is locked"). The rollback journal takes its locks through the file system, which the file
//! systems of clusters share between machines.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

/// The magic numbers of file systems that several machines mount at once (`statfs`): NFS,
/// SMB and CIFS, CephFS, Lustre, GPFS, BeeGFS, GlusterFS and others through FUSE, AFS, OCFS2,
/// GFS2, PanFS and 9P.
const SHARED: [i64; 14] = [
    0x6969,
    0x517b,
    0xfe53_4d42,
    0xff53_4d42,
    0x00c3_6400,
    0x0bd0_0bd0,
    0x4750_4653,
    0x1983_0326,
    0x6573_5546,
    0x5346_414f,
    0x7461_636f,
    0x0116_1970,
    0xaad7_aaea,
    0x0102_1997,
];

/// The journal mode for an SQLite file: `WAL`, or `DELETE` when its folder is on a file system
/// that machines share. A folder that cannot be asked counts as local.
pub fn journal_mode(file: &Path) -> &'static str {
    let folder = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    match rustix::fs::statfs(folder) {
        Ok(found) if shared(found.f_type as i64) => "DELETE",
        _ => "WAL",
    }
}

fn shared(kind: i64) -> bool {
    SHARED.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_of_shared_file_systems_get_the_rollback_journal() {
        assert!(shared(0x6969), "NFS");
        assert!(shared(0x00c3_6400), "CephFS");
        assert!(!shared(0xef53), "ext4");
        assert!(!shared(0x0102_1994), "tmpfs");
        let folder = tempfile::tempdir().expect("a folder");
        let local = journal_mode(&folder.path().join("runs.sqlite"));
        assert_eq!(local, "WAL");
    }
}
