//! nf-report and nf-crg-report against the reports that nf-report 1.1.1 and nf-crg-report 1.1.10
//! wrote with Nextflow 26.04.6 for the same probe pipeline; times, paths and hashes differ.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("{}", path.display()));
    serde_json::from_str(&text).expect("JSON")
}

fn keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

/// Runs the probe pipeline with this config in `folder`.
fn run(folder: &Path, config: &str) {
    common::copy(&common::fixtures("nf_report"), folder, &["main.nf"]);
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "report");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
}

#[test]
fn writes_three_reports() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-report@1.1.1' }}\nparams.greeting = 'hi'\nnfreport {{\n  \
         outputDir = '{}/reports'\n  prefix = 'pre-'\n  createLinkToLatestReport = true\n  \
         executionReport {{ enabled = true; format = ['json', 'tsv', 'html'] }}\n  \
         taskStatusReport {{ enabled = true; format = ['json', 'tsv', 'html'] }}\n  \
         sampleStatusReport {{ enabled = true; format = ['json', 'tsv', 'html']; \
         printCompletionSummary = true }}\n}}\n",
        folder.display()
    );
    run(folder, &config);
    let reports = folder.join("reports");
    let fixtures = common::fixtures("nf_report");
    let written =
        std::fs::read_to_string(reports.join("pre-sample-status-report.tsv")).expect("TSV");
    let recorded =
        std::fs::read_to_string(fixtures.join("pre-sample-status-report.tsv")).expect("TSV");
    let sorted = |text: &str| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    let tasks = json(&reports.join("pre-task-status-report.json"));
    assert_eq!(
        sorted(&written),
        sorted(&recorded),
        "samples in another order"
    );
    assert_eq!(
        tasks["summary"],
        serde_json::json!({ "total_tasks": 6, "completed": 4, "cached": 0, "failed": 0,
            "aborted": 0, "retried": 1, "failure_ignored": 1 })
    );
    assert_eq!(tasks["tasks_by_status"]["RETRIED"][0]["status"], "FAILED");
    let execution = json(&reports.join("pre-execution-report.json"));
    let recorded = json(&fixtures.join("pre-execution-report.json"));
    for place in 0..2 {
        assert_eq!(
            keys(&execution["events"][place]),
            keys(&recorded["events"][place])
        );
    }
    assert_eq!(
        execution["events"][1]["duration"]["units"],
        recorded["events"][1]["duration"]["units"]
    );
    assert_eq!(
        execution["events"][0]["params"],
        serde_json::json!({ "greeting": "hi" })
    );
    let link = reports.join("task-status-report-latest.tsv");
    assert_eq!(
        std::fs::read_link(&link).ok(),
        Some(reports.join("pre-task-status-report.tsv"))
    );
    let html =
        std::fs::read_to_string(reports.join("pre-sample-status-report.html")).expect("HTML");
    assert!(html.contains("<h1>Sample status report</h1>"), "{html}");
}

#[test]
fn names_meta_samples() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-report@1.1.1' }}\nnfreport {{\n  outputDir = '{}/reports2'\n  \
         suffix = '-b'\n  taskStatusReport {{ enabled = true; format = 'tsv'; \
         fields = ['task_name', 'status', 'tag'] }}\n  sampleStatusReport {{ enabled = true; \
         format = 'json'; extractSampleNameFrom = 'meta_map'; \
         sampleNameMetaKeyPattern = 'sample_id'; fields = ['task_name', 'status'] }}\n}}\n",
        folder.display()
    );
    run(folder, &config);
    let reports = folder.join("reports2");
    let samples = json(&reports.join("sample-status-report-b.json"));
    assert_eq!(
        samples["summary"],
        serde_json::json!({ "total_samples": 4, "completed": 3, "pending": 0,
            "partially_completed": 0, "failed": 1 })
    );
    assert_eq!(samples["samples_by_status"]["FAILED"][0]["sample_id"], "D");
    let task = &samples["samples_by_status"]["FAILED"][0]["tasks"][0];
    let mut fields = keys(task);
    fields.sort();
    assert_eq!(fields, ["status", "task_name"]);
    let tasks = std::fs::read_to_string(reports.join("task-status-report-b.tsv")).expect("TSV");
    assert!(tasks.starts_with("task_name\tstatus\ttag\n"), "{tasks}");
    assert!(tasks.contains("WORK (S3)\tFAILED\tS3\n"), "{tasks}");
}

/// Answers each request with the prices of the recorded fake, and counts the requests.
fn price_service() -> (u16, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let count = Arc::new(Mutex::new(0));
    let counted = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            *counted.lock().expect("the count") += 1;
            let body = r#"{"currency":"USD","kCPUHr":10.0,"kGBHr":1.0,"kGPUGBHr":12.0,"TBMonth":4.0} # comment"#;
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (port, count)
}

#[test]
fn adds_costs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(
        &common::fixtures("nf_report"),
        folder,
        &["prices.json", "launch_nf.sh"],
    );
    let config = format!(
        "plugins {{ id 'nf-crg-report@1.1.10' }}\nnfreport {{\n  outputDir = '{0}/reports'\n  \
         taskStatusReport {{ enabled = true; format = ['json', 'tsv'] }}\n  \
         sampleStatusReport {{ enabled = true }}\n  costs {{ priceJsonPath = '{0}/prices.json'; \
         launchFile = '{0}/launch_nf.sh'; defaultGpuMemGb = 16 }}\n}}\n",
        folder.display()
    );
    run(folder, &config);
    let reports = folder.join("reports");
    let mut names: Vec<String> = std::fs::read_dir(&reports)
        .expect("the reports")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    assert_eq!(names.len(), 3, "{names:?}");
    assert_eq!(names[0], "sample-status-report.json");
    assert!(
        names[1].starts_with("task-status-report-")
            && names[1].len() == "task-status-report-20261106113355.json".len()
    );
    let tasks = json(&reports.join(&names[1]));
    let summary = &tasks["summary"];
    assert_eq!(summary["has_head_job_cost"], true);
    assert_eq!(
        summary["pricing_rates"],
        serde_json::json!({ "currency": "EUR", "kCPUHr": 9.69, "kGBHr": 1.08, "kGPUGBHr": 11.63,
            "TBMonth": 3.3 })
    );
    let text = std::fs::read_to_string(reports.join(&names[1])).expect("the report");
    let after = text.split("\"exit_status\": ").nth(1).unwrap_or_default();
    let next = after.lines().nth(1).unwrap_or_default().trim();
    assert!(
        next.starts_with("\"compute_cost\": "),
        "costs follow exit_status: {next}"
    );
    let entry = &tasks["tasks_by_status"]["COMPLETED"][0];
    assert_eq!(entry["allocated_mem_gb"], "0.10 GB");
    assert_eq!(tasks["costs_by_process"]["WORK"]["count"], 4);
    let (port, requests) = price_service();
    let second = tempfile::tempdir().expect("a temporary directory");
    let folder = second.path();
    let config = format!(
        "plugins {{ id 'nf-crg-report@1.1.10' }}\nnfreport {{\n  outputDir = '{}/api'\n  \
         taskStatusReport {{ enabled = true; suffix = '' }}\n  \
         costs {{ priceAPI = 'http://127.0.0.1:{port}/fees' }}\n}}\n",
        folder.display()
    );
    run(folder, &config);
    let tasks = json(&folder.join("api/task-status-report.json"));
    assert_eq!(tasks["summary"]["pricing_rates"]["currency"], "USD");
    let cost = tasks["summary"]["total_estimated_cost"]
        .as_str()
        .unwrap_or_default();
    assert!(cost.starts_with('$'), "{cost}");
    assert_eq!(
        *requests.lock().expect("the count"),
        1,
        "the prices are fetched once a run"
    );
}
