//! Samples cached tasks for reruns and detects tools whose output digests change.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use task_model::Digest;
use workflow_graph::Verdict;

use crate::{CacheEntry, TaskResult};

/// The name of the standard output among the outputs that the check compares: drivers read it,
/// like Nextflow's `stdout` outputs and Argo's `result`, so it is part of the result.
pub const STANDARD_OUTPUT: &str = "standard output";

/// Whether the check runs the task of `key` again in `run`, for a share from 0 (none) to 1
/// (all). The choice depends only on the run and the key, so that a run chooses the same
/// tasks when it asks again, and each run chooses others.
pub(crate) fn chosen(share: f64, run: &str, key: &Digest) -> bool {
    if share <= 0.0 {
        return false;
    }
    if share >= 1.0 {
        return true;
    }
    let mixed = Digest::sha256(format!("{run}\n{}", key.hex()).as_bytes());
    // The first 16 hexadecimal digits are an evenly spread number.
    let number = u64::from_str_radix(&mixed.hex()[..16], 16).unwrap_or(u64::MAX);
    (number as f64) < share * (u64::MAX as f64)
}

/// The tool of a task: its image, pinned to a digest, and the name of its step without the
/// names of the workflows it is in, so that the same module in two Nextflow pipelines, like
/// `A:B:FASTQC` and `C:FASTQC`, is the same tool.
pub(crate) fn tool(image: &str, step: &str) -> Digest {
    let name = step.rsplit(':').next().unwrap_or(step);
    Digest::sha256(format!("{image}\n{name}").as_bytes())
}

/// What the task gave when it ran again, against the result in the cache.
pub(crate) fn verdict(earlier: &CacheEntry, now: &TaskResult) -> Verdict {
    match now.exit_code {
        Some(code) if code == earlier.exit_code => {}
        other => {
            return Verdict::Failed {
                exit_code: other.unwrap_or(-1),
            };
        }
    }
    let digests = |files: &[crate::StoredFile], stdout: &Option<Digest>| {
        let mut digests: BTreeMap<String, Option<Digest>> = files
            .iter()
            .map(|file| (file.path.clone(), Some(file.digest.clone())))
            .collect();
        digests.insert(STANDARD_OUTPUT.to_owned(), stdout.clone());
        digests
    };
    let before = digests(&earlier.files, &earlier.stdout);
    let after = digests(&now.files, &now.stdout);
    let differing: BTreeSet<&String> = (before.keys().chain(after.keys()))
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    let paths: Vec<String> = differing.into_iter().cloned().collect();
    match paths.is_empty() {
        true => Verdict::Same,
        false => Verdict::Differs { paths },
    }
}

/// What the mark of a tool in the store says: which task showed that it gives other bytes.
#[derive(Serialize)]
pub(crate) struct Note<'a> {
    pub image: &'a str,
    pub step: &'a str,
    /// The run and task that ran again, and the run whose result they were compared with.
    pub run: &'a str,
    pub task: &'a str,
    pub earlier_run: &'a str,
    pub differing: &'a [String],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredFile;

    fn file(path: &str, content: &[u8]) -> StoredFile {
        StoredFile {
            path: path.into(),
            digest: Digest::sha256(content),
            size: content.len() as u64,
            location: format!("runs/r/t/1/work/{path}"),
            directory: false,
        }
    }

    fn entry(files: Vec<StoredFile>) -> CacheEntry {
        CacheEntry {
            run: "earlier".into(),
            task: "T-1".into(),
            exit_code: 0,
            files,
            stdout: Some(Digest::sha256(b"out")),
            stderr: None,
            milliseconds: None,
            containers: Vec::new(),
        }
    }

    fn result(exit_code: i32, files: Vec<StoredFile>, stdout: &[u8]) -> TaskResult {
        TaskResult {
            exit_code: Some(exit_code),
            files,
            stdout: Some(Digest::sha256(stdout)),
            ..entry(Vec::new()).result()
        }
    }

    #[test]
    fn accepts_matching_bytes() {
        let earlier = entry(vec![file("a.txt", b"a"), file("b.txt", b"b")]);
        let now = result(0, vec![file("b.txt", b"b"), file("a.txt", b"a")], b"out");
        assert_eq!(verdict(&earlier, &now), Verdict::Same);
    }

    #[test]
    fn reports_content_differences() {
        let earlier = entry(vec![file("a.txt", b"a"), file("gone.txt", b"g")]);
        let now = result(
            0,
            vec![file("a.txt", b"A"), file("new.txt", b"n")],
            b"other",
        );
        let expected = ["a.txt", "gone.txt", "new.txt", STANDARD_OUTPUT].map(str::to_owned);
        assert_eq!(
            verdict(&earlier, &now),
            Verdict::Differs {
                paths: expected.to_vec()
            }
        );
        let failed = result(3, Vec::new(), b"");
        assert_eq!(verdict(&earlier, &failed), Verdict::Failed { exit_code: 3 });
    }

    #[test]
    fn samples_requested_share() {
        let key = |n: u32| Digest::sha256(&n.to_be_bytes());
        let chosen_of = |share: f64| (0..1000).filter(|n| chosen(share, "run", &key(*n))).count();
        assert_eq!(chosen_of(0.0), 0);
        assert_eq!(chosen_of(1.0), 1000);
        let tenth = chosen_of(0.1);
        assert!((60..140).contains(&tenth), "{tenth}");
        // The same run chooses the same tasks.
        assert_eq!(chosen(0.5, "run", &key(7)), chosen(0.5, "run", &key(7)));
    }

    #[test]
    fn identifies_step_tool() {
        let image = "quay.io/fastqc@sha256:ab";
        assert_eq!(tool(image, "A:B:FASTQC"), tool(image, "C:FASTQC"));
        assert_ne!(tool(image, "FASTQC"), tool(image, "MULTIQC"));
        assert_ne!(tool(image, "FASTQC"), tool("other@sha256:cd", "FASTQC"));
    }
}
