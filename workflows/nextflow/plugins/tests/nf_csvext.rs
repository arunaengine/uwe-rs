//! nf-csvext against the files and lines that nf-csvext 0.2.0 wrote with Nextflow 26.04.6 for
//! `fixtures/nf_csvext/main.nf`, and against its recorded error cases.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

/// The items of `csv_create` that the console printed on one line, each from its label on.
fn items(line: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for part in line.split("create ").filter(|part| !part.is_empty()) {
        items.push(format!("create {part}"));
    }
    items
}

/// Runs a script in a folder with the fixture files; gives the error and the printed text.
fn run(folder: &Path, script: &str) -> (Option<String>, String) {
    let fixtures = common::fixtures("nf_csvext");
    std::fs::create_dir_all(folder.join("data")).expect("makes the data folder");
    for name in ["a.csv", "b.csv", "narrow.csv"] {
        std::fs::copy(fixtures.join(name), folder.join("data").join(name)).expect("copies");
    }
    let config = "plugins { id 'nf-csvext@0.2.0' }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "csvext");
    (execution.error.map(|error| format!("{error:?}")), printed)
}

#[test]
fn writes_recorded_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let fixtures = common::fixtures("nf_csvext");
    let script = std::fs::read_to_string(fixtures.join("main.nf")).expect("the script");
    // uwe's path class has another simple name than Nextflow's `UnixPath`.
    let script = script.replace("p.getClass().simpleName", "'UnixPath'");
    let (error, printed) = run(directory.path(), &script);
    assert_eq!(error, None, "{printed}");
    let recorded = std::fs::read_to_string(fixtures.join("out-main.txt")).expect("the record");
    let (mut files, mut created) = (String::new(), Vec::new());
    for line in recorded.lines() {
        match line {
            line if line.starts_with("Downloading") || line.starts_with("[SUCCESS]") => {}
            line if line.starts_with("create ") => created.extend(items(line)),
            line => files.push_str(&format!("{line}\n")),
        }
    }
    let (mut heard, mut lines) = (String::new(), Vec::new());
    for line in printed.lines() {
        match line.starts_with("create ") {
            true => lines.push(line.to_owned()),
            false => heard.push_str(&format!("{line}\n")),
        }
    }
    assert_eq!(heard, files);
    created.sort();
    lines.sort();
    assert_eq!(lines, created);
}

#[test]
fn reports_errors() {
    let include = "include { csv_concat; csv_sort; csv_trim } from 'plugin/nf-csvext'\n";
    let a = "file(\"${projectDir}/data/a.csv\")";
    let cases = [
        (
            format!("csv_concat({a}, file(\"${{projectDir}}/data/narrow.csv\"), header: true)"),
            "source and append must to be same size",
        ),
        (
            format!("csv_trim({a}, column: 'a', columns: ['b'])"),
            "Only column or columns must to be specified, not both",
        ),
        (
            format!("csv_trim({a}, column: 'a,b')"),
            "Column argument can't be a list. Use instead `columns`",
        ),
        (format!("csv_trim({a})"), "Column(s) to trim are required"),
    ];
    for (call, message) in cases {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let script = format!("{include}workflow {{ println 'result: ' + {call}?.text }}\n");
        let (error, printed) = run(directory.path(), &script);
        let error = error.unwrap_or_default();
        assert!(error.contains(message), "{call}\n{error}\n{printed}");
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = format!(
        "{include}workflow {{ def r = csv_sort({a}, column: 'nope'); println 'result: ' + \
         r?.text }}\n"
    );
    let (error, printed) = run(directory.path(), &script);
    assert_eq!(error, None, "{printed}");
    assert_eq!(
        printed,
        "java.lang.IllegalArgumentException: Invalid column index nope\nresult: null\n"
    );
}
