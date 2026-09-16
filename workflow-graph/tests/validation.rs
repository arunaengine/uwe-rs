//! Native acceptance precedes cache settlement and is separate from process exit evidence.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use workflow_graph::{
    Attempt, Driver, DriverError, Engine, Event, ExecuteError, Executor, Language, Observer,
    Outcome, Running, Task, TaskId,
};

type Trace = Arc<Mutex<Vec<String>>>;

struct Checked {
    trace: Trace,
    accepted: bool,
    fail: bool,
    decision: Option<(TaskId, u32)>,
}

impl Driver for Checked {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(vec![Task {
            id: TaskId(1),
            ..Task::default()
        }])
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        assert_eq!(
            outcome.accepted, None,
            "executor evidence is not native acceptance"
        );
        self.trace.lock().unwrap().push("checked".into());
        self.decision = self.accepted.then_some((outcome.id, outcome.attempt));
        if self.fail {
            Err(DriverError::new("native output failure"))
        } else {
            Ok(Vec::new())
        }
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.decision.take() == Some((task, attempt))
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

struct Process(Trace);

impl Executor for Process {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            exit_code: 0,
            accepted: Some(true),
            ..Outcome::default()
        }))
    }

    fn settle(
        &mut self,
        task: TaskId,
        attempt: u32,
        accepted: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecuteError>> + Send + 'static>> {
        let trace = self.0.clone();
        Box::pin(async move {
            assert_eq!((task, attempt), (TaskId(1), 1));
            trace.lock().unwrap().push(format!("settled {accepted}"));
            Ok(())
        })
    }
}

struct Recorded(Trace);

impl Observer for Recorded {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Finished { outcome, .. } => {
                assert_eq!(outcome.exit_code, 0);
                assert_eq!(outcome.accepted, None);
                self.0.lock().unwrap().push("finished".into());
            }
            Event::Validated { accepted, .. } => {
                self.0.lock().unwrap().push(format!("validated {accepted}"));
            }
            _ => {}
        }
    }
}

#[test]
fn exit_zero_is_settled_only_after_native_validation_even_when_completion_fails() {
    for (accepted, fail) in [(true, false), (false, false), (false, true)] {
        let trace = Trace::default();
        let mut driver = Checked {
            trace: trace.clone(),
            accepted,
            fail,
            decision: None,
        };
        let mut executor = Process(trace.clone());
        let mut observer = Recorded(trace.clone());
        let execution = Engine::new(&mut executor)
            .observer(&mut observer)
            .run("checked", &mut driver);
        assert_eq!(execution.error.is_some(), fail);
        assert_eq!(execution.graph.nodes[0].exit_code, Some(0));
        assert_eq!(execution.graph.nodes[0].accepted, Some(accepted));
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "finished".to_owned(),
                "checked".to_owned(),
                format!("validated {accepted}"),
                format!("settled {accepted}"),
            ]
        );
    }
}
