//! Writes the report of a small recorded run and reads it back: one HTML file that refers to
//! nothing outside itself, with the data of the page and the end of each log.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_database::{Database, RunStart};
use run_page::{LOG_TAIL, report};
use serde_json::Value;
use workflow_graph::{Language, Outcome, OutputFile, Task, TaskId};

/// Records a simulated run of two tasks: the first prints a long log with text that looks like
/// the end of a script element, the second failed. The output of the first went to a folder.
fn record(directory: &std::path::Path) -> Database {
    let path = directory.join("runs.sqlite");
    let start = RunStart {
        id: "report-run".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into(), "main.nf".into()],
        parameters: Vec::new(),
        executor: "simulator".into(),
        simulated: true,
        store: Some("/cache/store".into()),
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start)
        .expect("starts");
    for (id, step) in [(1, "SAY"), (2, "SHOUT")] {
        let task = Task {
            id: TaskId(id),
            name: format!("{step} ({id})"),
            step: step.into(),
            command: vec!["bash".into(), "-c".into(), "echo hi".into()],
            outputs: vec!["out.txt".into()],
            ..Task::default()
        };
        recorder.requested(&task, &[]);
        recorder.started(TaskId(id), 1);
        let outcome = Outcome {
            id: TaskId(id),
            exit_code: if id == 1 { 0 } else { 3 },
            files: vec![OutputFile {
                path: "out.txt".into(),
                location: format!("/cache/store/{id}/out.txt"),
                ..OutputFile::default()
            }],
            stdout: format!("{}</script><b>the end\n", "x".repeat(3 * LOG_TAIL)),
            stderr: "a warning\n".into(),
            ..Outcome::default()
        };
        recorder.finished(&outcome, 1, false);
    }
    recorder.delivered("/work/results/out.txt", "/cache/store/1/out.txt");
    recorder.ended(Some("task SHOUT (2) failed"));
    recorder.close().expect("closes");
    Database::open(&path).expect("opens again")
}

/// The data of the report, from its JSON element.
fn data(html: &str) -> Value {
    let start = r#"<script type="application/json" id="report">"#;
    let from = html.find(start).expect("the data") + start.len();
    let to = from + html[from..].find("</script>").expect("its end");
    serde_json::from_str(&html[from..to]).expect("JSON")
}

#[test]
fn embeds_report_data() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = record(directory.path());
    assert_eq!(report(&database, "no-such-run").expect("reads"), None);
    let html = report(&database, "report-run")
        .expect("reads")
        .expect("a report");

    // Nothing outside the file: no linked style or script, no address but the name space of
    // SVG, which is no address that a browser loads.
    for outside in [
        "<link",
        "<script src",
        " src=",
        "@import",
        "url(//",
        "<iframe",
    ] {
        assert!(!html.contains(outside), "the report holds `{outside}`");
    }
    let addresses = html.matches("http://").count() + html.matches("https://").count();
    let names = html.matches("http://www.w3.org/").count();
    assert_eq!(addresses, names, "an address that is no name space");
    // The text of a log cannot end the element of the data.
    assert_eq!(html.matches("</script>").count(), 9);
    assert!(html.contains("\\u003c/script>\\u003cb>"));

    let data = data(&html);
    assert_eq!(data["run"], "report-run");
    let answers = &data["answers"];
    let run = &answers["/api/runs/report-run"];
    assert_eq!(run["run"]["state"], "failed");
    assert_eq!(run["tasks"].as_array().map(Vec::len), Some(2));
    let task = &answers["/api/runs/report-run/tasks/2"];
    assert_eq!(task["row"]["exit_code"], 3);
    assert_eq!(task["description"]["command"][2], "echo hi");
    assert_eq!(task["stderr"]["text"], "a warning\n");
    let stdout = &task["stdout"];
    assert_eq!(stdout["whole"], false);
    let text = stdout["text"].as_str().expect("a text");
    assert!(
        text.len() <= LOG_TAIL && text.ends_with("</script><b>the end\n"),
        "{text}"
    );
    assert!(answers["/api/runs/report-run/time"].is_object());
    assert!(answers.get("/api/runs/report-run/tasks/1/why").is_some());
    let pictures = data["pictures"].as_object().expect("pictures");
    assert_eq!(pictures.len(), 4);
    let svg = pictures["/api/runs/report-run/graph.svg?view=tasks&direction=down"]
        .as_str()
        .expect("a picture");
    assert!(svg.contains("<svg") && svg.contains("SHOUT"), "{svg}");
    assert_eq!(data["deliveries"][0]["target"], "/work/results/out.txt");
    assert_eq!(data["deliveries"][0]["task"], 1);
}
