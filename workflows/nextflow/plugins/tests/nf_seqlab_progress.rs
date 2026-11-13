//! nf-seqlab-progress against the dashboard states that nf-seqlab-progress 0.2.0 wrote with
//! Nextflow 26.04.6 for its validation pipeline, read from the audit file of real uwe runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

/// The validation pipeline of the plugin without its progress snapshots.
const SCRIPT: &str = "include { registerProgressInputs; registerProgressStages } from 'plugin/nf-seqlab-progress'\n\
    process EXACT_PROGRESS {\n  tag 'chr22'\n  input: val meta\n  output: path 'done.txt'\n  script:\n  \
    \"\"\"\n  if [ '${params.scenario}' = failure ]; then exit 42; fi\n  touch done.txt\n  \"\"\"\n}\n\
    workflow {\n  registerProgressInputs([[file_id: 'chr22', path: 'synthetic://chr22']])\n  \
    registerProgressStages([[id: 'exact', label: 'Exact progress', file_ids: ['chr22']]],\n    \
    [[process: 'EXACT_PROGRESS', stage: 'exact', completion_boundary: 'parent']])\n  \
    EXACT_PROGRESS(channel.value([file_id: 'chr22', parent_file_id: 'chr22']))\n}\n";

/// Runs the pipeline in `folder`; the error and the dashboard states of the audit file.
fn run(folder: &Path, scenario: &str, mode: &str) -> (Option<String>, Vec<serde_json::Value>) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-seqlab-progress@0.2.0' }}\nparams {{ outdir = '{}/results'; \
         progress_mode = '{mode}'; scenario = '{scenario}' }}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, &format!("progress-{scenario}"));
    let audit = std::fs::read_to_string(folder.join("results/pipeline_info/progress.jsonl"));
    let states = (audit.unwrap_or_default().lines())
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    (execution.error.map(|error| error.to_string()), states)
}

#[test]
fn audits_dashboard_states() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (error, states) = run(directory.path(), "success", "off");
    assert!(error.is_none(), "{error:?}");
    let last = states.last().expect("states");
    assert_eq!(
        last["stage"],
        serde_json::json!({ "id": "exact", "label": "Exact progress", "state": "completed",
            "completed_files": 1, "expected_files": 1, "percent": 100.0 })
    );
    assert_eq!(last["schema"], "nf-seqlab.dashboard/v1");
    assert_eq!(last["run_id"], "progress-success");
    let running = states
        .iter()
        .find(|state| state["active_files"][0]["file_id"] == "chr22");
    let running = running.expect("a state with the running file");
    assert_eq!(
        running["active_files"][0]["percent"],
        serde_json::Value::Null
    );
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (error, states) = run(directory.path(), "failure", "plain");
    assert!(error.is_some(), "the task fails");
    let last = states.last().expect("states");
    assert_eq!(last["stage"]["state"], "failed");
    assert_eq!(last["error_count"], 1);
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (error, _) = run(directory.path(), "success", "fancy");
    let error = error.unwrap_or_default();
    assert!(
        error.contains("Unknown progress mode 'fancy'; expected auto, full, compact, minimal, plain, json, or off"),
        "{error}"
    );
}
