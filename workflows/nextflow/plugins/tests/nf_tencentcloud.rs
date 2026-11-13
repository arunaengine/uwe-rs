//! nf-tencentcloud's metadata file of a run: the run, its params and the events of its tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

#[test]
fn writes_run_metadata() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let script = "process BIG {\n output: path 'out.txt'\n script:\n 'echo big > out.txt'\n}\n\
                  workflow { BIG() }\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    // The launch directory of these runs is the folder of the crate, so the file is absolute.
    let config = format!(
        "plugins {{ id 'nf-tencentcloud@2.0.1' }}\nparams.sample = 'probe'\n\
         tencentcloud {{\n metadata {{\n  enabled = true\n  file = '{}/meta/metadata.json'\n \
         }}\n}}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "probe_run");
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let text = std::fs::read_to_string(folder.join("meta/metadata.json")).expect("the file");
    let metadata: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(metadata["runName"], "probe_run");
    assert_eq!(metadata["event"], "completed");
    assert_eq!(metadata["metadata"]["parameters"]["sample"], "probe");
    let time = metadata["utcTime"].as_str().expect("a time");
    assert!(time.ends_with('Z') && time.len() == 20, "{time}");
    let events: Vec<&str> = (metadata["trace"].as_array().expect("a trace").iter())
        .filter_map(|entry| entry["event"].as_str())
        .collect();
    assert_eq!(
        events,
        ["process_submitted", "process_started", "process_completed"]
    );
    assert_eq!(metadata["trace"][2]["trace"]["process"], "BIG");
}
