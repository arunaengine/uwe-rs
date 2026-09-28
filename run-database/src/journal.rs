//! The journal of the SQLite files of iunctus: WAL on a local file system, where readers go on
//! while a run writes, and the rollback journal on a file system that machines share, like NFS
//! or CephFS on a cluster. WAL keeps its index in memory that all its processes share, so they
//! must be on one machine: a run on a second node of a cluster cannot open the file ("database
//! is locked"). The rollback journal takes its locks through the file system, which the file
//! systems of clusters share between machines.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

/// The magic numbers of file systems that several machines mount at once (`statfs`): NFS,
/// SMB and CIFS, CephFS, Lustre, GPFS, BeeGFS, GlusterFS and others through FUSE, AFS, OCFS2,
/// GFS2, PanFS and 9P.
const SHARED: [u32; 14] = [
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

/// How an SQLite file keeps its journal, and how long a connection waits for another process
/// that holds the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    /// `WAL` or `DELETE`, for `PRAGMA journal_mode`.
    pub mode: &'static str,
    pub wait: Duration,
}

impl Journal {
    /// WAL on a local file system.
    pub const LOCAL: Journal = Journal {
        mode: "WAL",
        wait: Duration::from_secs(10),
    };
    /// The rollback journal on a file system that machines share. Its locks are slow there,
    /// and SQLite does not serve waiting processes in order, so a process may wait for many
    /// commits of others (on CephFS, 4 processes of 30 small commits each waited up to 8 s).
    pub const SHARED: Journal = Journal {
        mode: "DELETE",
        wait: Duration::from_secs(300),
    };
}

/// The journal for an SQLite file: [`Journal::SHARED`] when its folder is on a file system
/// that machines share, else [`Journal::LOCAL`], using the closest observable ancestor when
/// the file's parent does not exist yet.
pub fn journal(file: &Path) -> Journal {
    let folder = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    journal_at(folder, |folder| {
        rustix::fs::statfs(folder)
            .ok()
            .map(|found| found.f_type as u32)
    })
}

fn journal_at(folder: &Path, kind: impl Fn(&Path) -> Option<u32>) -> Journal {
    let found = folder
        .ancestors()
        .filter(|folder| !folder.as_os_str().is_empty())
        .chain(std::iter::once(Path::new(".")))
        .find_map(kind);
    match found {
        Some(kind) if shared(kind) => Journal::SHARED,
        _ => Journal::LOCAL,
    }
}

fn shared(kind: u32) -> bool {
    SHARED.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_uses_rollback() {
        assert!(shared(0x6969), "NFS");
        assert!(shared(0x00c3_6400), "CephFS");
        assert!(!shared(0xef53), "ext4");
        assert!(!shared(0x0102_1994), "tmpfs");
        let folder = tempfile::tempdir().expect("a folder");
        assert_eq!(journal(&folder.path().join("runs.sqlite")), Journal::LOCAL);
    }

    #[test]
    fn inspects_existing_parent() {
        let folder = Path::new("/shared/new/deep");
        assert_eq!(
            journal_at(folder, |path| (path == Path::new("/shared"))
                .then_some(0x6969)),
            Journal::SHARED
        );
        assert_eq!(
            journal_at(folder, |path| {
                if path == Path::new("/shared/new") {
                    Some(0xef53)
                } else if path == Path::new("/shared") {
                    Some(0x6969)
                } else {
                    None
                }
            }),
            Journal::LOCAL
        );
        assert_eq!(
            journal_at(Path::new("new/deep"), |path| (path == Path::new("."))
                .then_some(0x00c3_6400)),
            Journal::SHARED
        );
    }

    /// Runs of several processes record into one database with the rollback journal, as on a
    /// file system that cluster nodes share: none of them fails with "database is locked".
    #[test]
    fn records_concurrent_transactions() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("runs.sqlite");
        let runs: Vec<_> = (0..4)
            .map(|number| {
                let path = path.clone();
                std::thread::spawn(move || -> Result<(), crate::DatabaseError> {
                    let database = crate::Database::open_as(&path, Journal::SHARED)?;
                    let mut recorder = database.record(&start(number))?;
                    for id in 1..=20 {
                        let task = workflow_graph::Task {
                            id: workflow_graph::TaskId(id),
                            name: format!("MAKE ({id})"),
                            step: "MAKE".into(),
                            ..workflow_graph::Task::default()
                        };
                        recorder.requested(&task, &[]);
                        recorder.started(task.id, 1);
                        std::thread::sleep(Duration::from_millis(15));
                        let outcome = workflow_graph::Outcome {
                            id: task.id,
                            ..workflow_graph::Outcome::default()
                        };
                        recorder.finished(&outcome, 1, false);
                    }
                    recorder.ended(None);
                    recorder.close()
                })
            })
            .collect();
        for run in runs {
            run.join().expect("the run ends").expect("records the run");
        }
        let connection = rusqlite::Connection::open(&path).expect("opens the file");
        let mode: String = (connection.query_row("PRAGMA journal_mode", [], |row| row.get(0)))
            .expect("the journal mode");
        assert_eq!(mode, "delete");
    }

    fn start(number: u32) -> crate::RunStart {
        crate::RunStart {
            id: format!("run-{number}"),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: workflow_graph::Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "slurm".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        }
    }
}
