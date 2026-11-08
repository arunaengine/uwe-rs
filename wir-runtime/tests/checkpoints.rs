//! Replans a dynamic region through the adapter of its planner: the first plan, and a new
//! plan once a checkpoint instance ended, which the planner hears on a later call.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json, json};
use wir::Status;
use wir::reserved::{Identity, PlanUpdate};
use wir_runtime::adapter::{CallId, Changes, Ending, RegionCall, TaskAdapter, TaskCall};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, DriverError, Launch, Outcome, Task, TaskId};

/// A plan of `revision` with one job op of each name.
fn plan(revision: u64, jobs: &[&str], final_plan: bool) -> PlanUpdate {
    let ops: Vec<Json> = (jobs.iter())
        .map(|job| {
            let builder =
                json!({"snakemake_job": {"task": {"source": "smk", "entry": "all", "call": job}}});
            json!({"path": job, "kind": {"task": {"builder": builder}}})
        })
        .collect();
    let update = json!({"revision": revision, "graph": {"name": "jobs", "ops": ops},
        "final_plan": final_plan});
    serde_json::from_value(update).unwrap()
}

/// Plans the job `check`, then `check` and `after` once `check` ended; runs each job as one
/// task, and records what it heard.
struct Planning {
    heard: Arc<Mutex<Vec<String>>>,
    calls: HashMap<TaskId, CallId>,
}

impl TaskAdapter for Planning {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let task = Task {
            id: TaskId(self.calls.len() as u64 + 1),
            name: call.frame.path.clone(),
            ..Task::default()
        };
        self.calls.insert(task.id, call.call);
        Ok(Changes {
            tasks: vec![(call.call, task)],
            ..Changes::default()
        })
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let ending = Ending::Succeeded {
            values: Map::new(),
            empty: Vec::new(),
        };
        Ok(Changes {
            ended: vec![(self.calls[&outcome.id], ending)],
            ..Changes::default()
        })
    }

    fn plan(&mut self, _: &RegionCall) -> Result<PlanUpdate, DriverError> {
        Ok(plan(0, &["check"], false))
    }

    fn replan(
        &mut self,
        _: CallId,
        instance: &Identity,
        status: Status,
    ) -> Result<Option<PlanUpdate>, DriverError> {
        self.heard
            .lock()
            .unwrap()
            .push(format!("{instance:?} {status:?}"));
        Ok(match instance {
            Identity::Named(name) if name == "check" => Some(plan(1, &["check", "after"], true)),
            _ => None,
        })
    }
}

#[test]
fn checkpoint_replans() {
    let folder = tempfile::tempdir().unwrap();
    let planner = json!({"snakemake": {"source": "smk", "targets": ["all"]}});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "smk", "language": "snakemake", "location": "Snakefile", "digest": "sha256:ab"}],
        "graphs": [{"name": "main", "ops": [{"path": "jobs", "kind": {"dynamic": {"planner": planner}}}]}],
        "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("out")),
        ..Launch::default()
    };
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    let heard = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&heard);
    settings.adapters.register("snakemake_job", move |_| {
        let heard = Arc::clone(&shared);
        Ok(Box::new(Planning {
            heard,
            calls: HashMap::new(),
        }))
    });
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let first = driver.start().unwrap();
    assert_eq!(first.len(), 1, "the first plan runs check: {first:?}");
    let done = |task: &Task| Outcome {
        id: task.id,
        attempt: 1,
        ..Outcome::default()
    };
    let after = driver.complete(done(&first[0])).unwrap();
    assert!(
        after.is_empty(),
        "the planner hears of check after the call it ended in"
    );
    assert!(heard.lock().unwrap().is_empty());
    assert!(
        driver.wake_at().is_some(),
        "the driver wakes to tell the planner"
    );
    let replanned = driver.wake().unwrap();
    let names: Vec<&str> = replanned.iter().map(|task| task.name.as_str()).collect();
    assert_eq!(
        names,
        ["jobs/after: jobs/after"],
        "check stays, after is added"
    );
    driver.complete(done(&replanned[0])).unwrap();
    while driver.wake_at().is_some() {
        assert!(driver.wake().unwrap().is_empty());
    }
    driver.finish().unwrap();
    let heard = heard.lock().unwrap();
    assert_eq!(heard[0], "Named(\"check\") Succeeded");
}
