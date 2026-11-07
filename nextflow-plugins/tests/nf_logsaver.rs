//! nf-logsaver against the files that nf-logsaver 0.1.0 copied with Nextflow 26.04.6; their
//! names end in the task hash, which differs in uwe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn copies_task_logs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_logsaver");
    common::copy(&fixtures, folder, &["main.nf"]);
    let config = format!(
        "plugins {{ id 'nf-logsaver@0.1.0' }}\nparams.outdir = '{}/out'\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "logs");
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert!(printed.contains("Hello, probe!\n"), "{printed}");
    let saved = folder.join("out/logsaver_command_files");
    let without_hash = |name: &str| {
        let (base, extension) = name.rsplit_once('.').unwrap_or_default();
        let base = base.rsplit_once('_').map_or(base, |(base, _)| base);
        format!("{base}.{extension}")
    };
    let contents = |folder: &std::path::Path| -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = std::fs::read_dir(folder)
            .expect("the folder")
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(entry.path()).ok()?;
                (name != "main.nf").then(|| (without_hash(&name), text))
            })
            .collect();
        files.sort();
        files
    };
    assert_eq!(contents(&saved), contents(&fixtures));
}
