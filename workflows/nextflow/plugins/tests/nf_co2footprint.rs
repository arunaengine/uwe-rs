//! nf-co2footprint against the files that nf-co2footprint 1.4.1 wrote with Nextflow 26.04.6;
//! the probe's tasks report its recorded run time and CPU share.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;
use std::time::Duration;

use common::Timing;
use serde_json::Value;

/// The task of the recording: 1007 ms of run time at 2 % of one CPU.
const TIMING: Timing = Timing {
    realtime: Duration::from_millis(1007),
    cpu: Duration::from_micros(20_140),
};

/// Runs the probe with the recorded config and `extra`, and gives the folder of its files.
/// The config gives the carbon intensity of the zone `DE` that the recording used, as uwe
/// ships no table of zones; the probe's tasks do not sleep, as their time is fixed.
fn run(folder: &Path, extra: &str) -> std::path::PathBuf {
    common::copy(&common::fixtures("nf_co2footprint"), folder, &["main.nf"]);
    let files = folder.join("co2");
    let config = format!(
        "plugins {{ id 'nf-co2footprint@1.4.1' }}\nco2footprint {{\n    \
         trace {{ file = '{f}/trace.txt' }}\n    \
         summary {{ enabled = true; file = '{f}/summary.txt' }}\n    \
         report {{ file = '{f}/report.html' }}\n    \
         provenance {{ file = '{f}/provenance.json' }}\n    \
         location = 'DE'\n    ci = 333.77\n{extra}}}\n",
        f = files.display(),
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run_timed(folder, "astonishing_wilson", Some(TIMING));
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    files
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixtures("nf_co2footprint").join(name)).expect("a fixture")
}

/// The trace lines that the recording's line of its third task gives for tasks 1 to 3, in
/// uwe's completion order, with no CPU model, which uwe does not know.
fn expected_trace(recorded: &str) -> String {
    let mut lines = recorded.lines();
    let mut text = format!("{}\n", lines.next().unwrap_or_default());
    let third = lines.next().unwrap_or_default();
    let third = third.replace("AMD Ryzen 9 7945HX with Radeon Graphics", "-");
    let rest = third.strip_prefix("3\t").unwrap_or_default();
    for task in 1..=3 {
        let rest = rest.replace("WORK (3)", &format!("WORK ({task})"));
        text.push_str(&format!(
            "{task}\t{}\n",
            rest.replace("CACHED", "COMPLETED")
        ));
    }
    text
}

#[test]
fn writes_trace_summary() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let files = run(directory.path(), "");
    let trace = std::fs::read_to_string(files.join("trace.txt")).expect("the trace");
    assert_eq!(trace, expected_trace(&fixture("co2footprint_trace.txt")));
    let summary = std::fs::read_to_string(files.join("summary.txt")).expect("the summary");
    let recorded = fixture("co2footprint_summary.txt");
    let (lines, recorded): (Vec<&str>, Vec<&str>) =
        (summary.lines().collect(), recorded.lines().collect());
    assert_eq!(lines.len(), recorded.len(), "{summary}");
    // The engine's own share differs, so the totals do; the file paths are the test's.
    let differ = [1, 2, 6, 7, 8, 26, 27, 28, 29];
    for (index, (line, recorded)) in lines.iter().zip(&recorded).enumerate() {
        if differ.contains(&index) {
            // The words before the value: up to the colon, else up to the first digit.
            let prefix = |line: &str| {
                let end = line
                    .find(':')
                    .or_else(|| line.find(|c: char| c.is_ascii_digit()));
                line[..end.unwrap_or(line.len())].to_owned()
            };
            assert_eq!(prefix(line), prefix(recorded), "line {index}");
        } else {
            assert_eq!(line, recorded, "line {index}");
        }
    }
}

#[test]
fn writes_provenance() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let files = run(directory.path(), "");
    let text = std::fs::read_to_string(files.join("provenance.json")).expect("the provenance");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    let recorded: Value =
        serde_json::from_str(&fixture("co2footprint_provenance.json")).expect("JSON");
    for key in [
        "@context",
        "@version",
        "prov:wasGeneratedBy",
        "@id",
        "@type",
        "workflowLevel",
    ] {
        assert_eq!(written[key], recorded[key], "{key}");
    }
    let ids = |node: &Value| -> Vec<String> {
        let mut ids = vec![node["@id"].as_str().unwrap_or_default().to_owned()];
        for part in node["hasPart"].as_array().into_iter().flatten() {
            let child = part["@id"].as_str().unwrap_or_default();
            ids.push(child.to_owned());
            for task in part["hasPart"].as_array().into_iter().flatten() {
                ids.push(task["@id"].as_str().unwrap_or_default().to_owned());
                for leaf in task["hasPart"].as_array().into_iter().flatten() {
                    ids.push(leaf["@id"].as_str().unwrap_or_default().to_owned());
                }
            }
        }
        ids.sort();
        ids
    };
    assert_eq!(ids(&written), ids(&recorded));
    let task = |root: &Value, id: &str| -> Value {
        let tasks = root["hasPart"][1]["hasPart"][0]["hasPart"]
            .as_array()
            .cloned();
        (tasks.unwrap_or_default().into_iter())
            .find(|task| task["@id"] == format!("urn:co2footprint:{id}"))
            .expect("the task")
    };
    let (mine, theirs) = (task(&written, "3"), task(&recorded, "3"));
    let keys = |task: &Value| -> Vec<String> {
        let keys = task
            .as_object()
            .map(|map| map.keys().cloned().collect::<Vec<_>>());
        keys.unwrap_or_default()
    };
    let without_model: Vec<String> = keys(&theirs)
        .into_iter()
        .filter(|key| key != "cpu_model")
        .collect();
    assert_eq!(
        keys(&mine),
        without_model,
        "uwe does not know the CPU model"
    );
    for key in [
        "task_id",
        "energy_consumption",
        "CO2e",
        "carbon_intensity",
        "%cpu",
        "memory",
        "realtime",
        "cpus",
        "pue",
        "powerdraw_cpu",
    ] {
        assert_eq!(
            mine[key]["additionalProperty"], theirs[key]["additionalProperty"],
            "{key}"
        );
        assert_eq!(mine[key]["@type"], theirs[key]["@type"], "{key}");
        assert_eq!(mine[key]["unitText"], theirs[key]["unitText"], "{key}");
    }
    let order = |text: &str| -> Vec<String> {
        (text.lines())
            .filter_map(|line| line.strip_prefix("    \""))
            .filter_map(|rest| rest.split_once('"').map(|(key, _)| key.to_owned()))
            .collect()
    };
    let recorded_order: Vec<String> = (order(&fixture("co2footprint_provenance.json")).into_iter())
        .filter(|key| key != "cpu_model")
        .collect();
    assert_eq!(
        order(&text),
        recorded_order,
        "the keys of the root in order"
    );
    let report = std::fs::read_to_string(files.join("report.html")).expect("the report");
    assert!(
        report
            .contains("<title>[astonishing_wilson] Nextflow CO\u{2082}e Footprint Report</title>")
    );
    assert!(
        report.contains("window.data = {\"trace\":[{\"task_id\""),
        "{report}"
    );
}

#[test]
fn uses_local_machines() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let files = run(directory.path(), "    machineType = 'local'\n");
    let trace = std::fs::read_to_string(files.join("trace.txt")).expect("the trace");
    assert_eq!(trace, expected_trace(&fixture("local-trace.txt")));
    let summary = std::fs::read_to_string(files.join("summary.txt")).expect("the summary");
    assert!(summary.contains("\n  machineType: local\n  powerdrawCpuDefault: null\n  powerdrawMem: 0.3725\n  pue: 1\n"), "{summary}");
}

#[test]
fn refuses_unknown_machines() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_co2footprint"), folder, &["main.nf"]);
    let config =
        "plugins { id 'nf-co2footprint@1.4.1' }\nco2footprint { machineType = 'laptop' }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "run");
    let error = execution
        .error
        .map(|error| error.to_string())
        .unwrap_or_default();
    let message = "`machineType?` 'laptop' is not supported. Please chose one of [local, compute \
                   cluster, cloud].";
    assert!(error.contains(message), "{error}");
}
