//! nf-storagemetrics against the format that nf-storagemetrics 0.1.1 wrote with Nextflow
//! 26.04.6; uwe measures the folder that holds a task's outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn measures_task_folders() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_storagemetrics"), folder, &["main.nf"]);
    let metrics = folder.join("metrics/storage.txt");
    let config = format!(
        "plugins {{ id 'nf-storagemetrics@0.1.1' }}\nstoragemetrics.file = '{}'\n",
        metrics.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "festering_koch");
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert!(printed.contains("Hello, probe!\n"), "{printed}");
    let text = std::fs::read_to_string(&metrics).expect("the metrics");
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("task_id\thash\tprocess\ttag\tstatus\tworkdir\tsize_bytes\tsize_human")
    );
    let mut tags = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        let [_, hash, process, tag, status, workdir, size, human] = cells[..] else {
            panic!("eight columns: {line}");
        };
        assert!(hash.len() == 9 && hash.as_bytes()[2] == b'/', "{hash}");
        assert_eq!((process, status), ("MAKE", "COMPLETED"));
        assert!(std::path::Path::new(workdir).is_dir(), "{workdir}");
        let size: u64 = size.parse().expect("bytes");
        let made: u64 = tag.trim_start_matches('t').parse::<u64>().expect("a tag") * 1500;
        assert!(size >= made, "{line}");
        assert!(human.ends_with('B'), "{human}");
        tags.push(tag.to_owned());
    }
    tags.sort();
    assert_eq!(tags, ["t1", "t1000", "t2"]);
}
