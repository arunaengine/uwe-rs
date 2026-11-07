//! nf-metalog against the CSV and report data that nf-metalog 0.2.0 wrote with Nextflow
//! 26.04.6 for the same pipeline, and its SQLite store as its specification describes it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixtures("nf_metalog").join(name)).expect("a fixture")
}

/// The fields that uwe does not know, which its CSV leaves out; uwe also gives `cpus` only
/// when the process sets it, where Nextflow gives 1.
const UNKNOWN: [&str; 3] = ["native_id", "module", "workdir"];

fn run(folder: &Path, store: &str) -> (String, String) {
    common::copy(&common::fixtures("nf_metalog"), folder, &["main.nf"]);
    let config = format!(
        "plugins {{ id 'nf-metalog@0.2.0' }}\nmetalog {{\n{store}    report {{ csvFile = \
         '{f}/metalog.csv'; htmlFile = '{f}/metalog.html'; overwrite = true }}\n}}\n",
        f = folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "shrivelled_coulomb");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let read = |name: &str| std::fs::read_to_string(folder.join(name)).expect("a report");
    (read("metalog.csv"), read("metalog.html"))
}

/// The rows of a CSV by task name: process, sample, tag, status, exit and attempt.
fn tasks(csv: &str) -> Vec<String> {
    let header: Vec<&str> = csv
        .split("\r\n")
        .next()
        .unwrap_or_default()
        .split(',')
        .collect();
    let column = |name: &str| header.iter().position(|known| *known == name).expect(name);
    // Only the script field is quoted; it holds commas only inside its quotes.
    let mut rows: Vec<String> = Vec::new();
    for record in csv
        .split("\r\n")
        .skip(1)
        .filter(|record| !record.is_empty())
    {
        let mut cells = Vec::new();
        let mut rest = record;
        while !rest.is_empty() {
            if let Some(quoted) = rest.strip_prefix('"') {
                let end = quoted.find("\",").unwrap_or(quoted.len() - 1);
                cells.push(quoted[..end].to_owned());
                rest = quoted.get(end + 2..).unwrap_or_default();
            } else {
                let (cell, next) = rest.split_once(',').unwrap_or((rest, ""));
                cells.push(cell.to_owned());
                rest = next;
            }
        }
        let cell = |name: &str| cells.get(column(name)).cloned().unwrap_or_default();
        rows.push(
            [
                "name", "process", "group_id", "tag", "status", "exit", "attempt",
            ]
            .map(cell)
            .join(" "),
        );
    }
    rows.sort();
    rows
}

#[test]
fn writes_memory_reports() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (csv, html) = run(directory.path(), "");
    let recorded = fixture("metalog.csv");
    let header = |csv: &str| csv.split("\r\n").next().unwrap_or_default().to_owned();
    let recorded_header = header(&recorded);
    let known: Vec<&str> = (recorded_header.split(','))
        .filter(|column| !UNKNOWN.contains(column))
        .collect();
    assert_eq!(header(&csv), known.join(","));
    assert_eq!(tasks(&csv), tasks(&recorded));
    let data = fixture("report-data.js");
    let cols = data
        .split("\"cols\":")
        .nth(1)
        .and_then(|rest| rest.split(']').next());
    let cols = cols.unwrap_or_default().replace(",\"workdir\"", "");
    assert!(
        html.contains(&format!("window.nfMetalogData = {{\"cols\":{cols}]")),
        "{html}"
    );
    assert!(html.contains("<title>[shrivelled_coulomb] nf-metalog report</title>"));
}

#[test]
fn writes_sqlite_reports() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = directory.path().join("sql/metalog.db");
    let store = format!(
        "    storageBackend = 'sqlite'\n    sqlite {{ file = '{}' }}\n",
        database.display()
    );
    let (csv, _) = run(directory.path(), &store);
    assert!(
        csv.starts_with("run_name,group_id,task_id,hash,process,tag,name,status,exit,"),
        "{csv}"
    );
    assert_eq!(tasks(&csv), tasks(&fixture("metalog.csv")));
    assert!(database.is_file());
}
