//! Runs dynamic regions through the driver: a plan given as a value input, its instances,
//! and the outputs of the final plan.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Running, execute};

/// Runs no task: the plans here hold exprs only.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// Runs the region `r` with the plan `plan` and the input `n`; its output `out`, or why the
/// run failed.
fn run(plan: Json, n: i64) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    let int = |name: &str| json!({"name": name, "type": "int"});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plan"}},
        "inputs": [{"name": "plan", "type": "dynamic"}, int("n")], "outputs": [int("out")]});
    let main = json!({"name": "main", "inputs": [{"name": "plan", "type": "dynamic"}, int("n")],
        "outputs": [int("out")], "ops": [op],
        "edges": [edge((None, "plan"), (Some("r"), "plan")), edge((None, "n"), (Some("r"), "n")),
            edge((Some("r"), "out"), (None, "out"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"plan": plan, "n": n});
    std::fs::write(folder.path().join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        inputs: Some(folder.path().join("inputs.json")),
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).map_err(|error| error.message)?;
    execute("regions", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap())
}

/// A plan of `revision` whose op `inc` gives `n + 1` as `out`.
fn plan(revision: u64, final_plan: bool) -> Json {
    let int = |name: &str| json!({"name": name, "type": "int"});
    let inc = json!({"path": "inc", "kind": {"expr": {"language": "groovy", "code": "inputs.n + 1"}},
        "inputs": [int("n")], "outputs": [int("out")]});
    let graph = json!({"name": "plan", "inputs": [int("n")], "outputs": [int("out")], "ops": [inc],
        "edges": [edge((None, "n"), (Some("inc"), "n")), edge((Some("inc"), "out"), (None, "out"))]});
    json!({"revision": revision, "graph": graph, "final_plan": final_plan})
}

#[test]
fn region_plans() {
    assert_eq!(run(plan(0, true), 4).unwrap(), json!({"out": 5}));
    let open = run(plan(0, false), 4).unwrap_err();
    assert!(open.contains("its plan never settled"), "{open}");
    let late = run(plan(1, true), 4).unwrap_err();
    assert!(late.contains("the first plan has revision 1"), "{late}");
}
