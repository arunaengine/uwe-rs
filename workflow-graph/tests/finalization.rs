//! Checks failure-report delivery after cancellation and secondary hook errors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use workflow_graph::{
    Attempt, CancellationToken, Delivery, DeliveryMode, Driver, DriverError, Engine, ExecuteError,
    Executor, Files, Language, Outcome, Running, Task,
};

struct Reports {
    pending: bool,
    delivered: usize,
}

impl Driver for Reports {
    fn language(&self) -> Language {
        Language::Nextflow
    }
    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Err(DriverError::new("original failure"))
    }
    fn complete(&mut self, _: Outcome) -> Result<Vec<Task>, DriverError> {
        unreachable!()
    }
    fn finish(&mut self) -> Result<(), DriverError> {
        unreachable!()
    }
    fn failed(&mut self, _: &ExecuteError) -> Result<(), DriverError> {
        self.pending = true;
        Err(DriverError::new("hook failure"))
    }
    fn deliveries(&mut self) -> Vec<Delivery> {
        if !std::mem::take(&mut self.pending) {
            return Vec::new();
        }
        vec![Delivery {
            location: "report".into(),
            target: "result".into(),
            mode: DeliveryMode::Copy,
            overwrite: true,
        }]
    }
    fn delivered(&mut self, _: &Delivery) -> Result<(), DriverError> {
        self.delivered += 1;
        Ok(())
    }
}

struct ReportFiles {
    failed: bool,
    calls: AtomicUsize,
}

impl Files for ReportFiles {
    fn local(&self, location: &str) -> io::Result<PathBuf> {
        Ok(location.into())
    }
    fn deliver(&self, _: &Delivery) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failed {
            Err(io::Error::other("delivery failure"))
        } else {
            Ok(())
        }
    }
}

struct ReportsExecutor(Arc<ReportFiles>);

impl Executor for ReportsExecutor {
    fn run(&mut self, _: Attempt) -> Running {
        unreachable!()
    }
    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(self.0.clone())
    }
}

#[test]
fn delivers_failure_reports() {
    for cancelled in [false, true] {
        for failed in [false, true] {
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            let files = Arc::new(ReportFiles {
                failed,
                calls: AtomicUsize::new(0),
            });
            let mut driver = Reports {
                pending: false,
                delivered: 0,
            };
            let execution = Engine::new(&mut ReportsExecutor(files.clone()))
                .cancel(cancel)
                .run("failure reports", &mut driver);
            let error = execution.error.unwrap();
            if cancelled {
                assert!(matches!(error, ExecuteError::Cancelled));
            } else {
                assert_eq!(error.to_string(), "original failure");
            }
            let hook = execution.failure_hook_error.unwrap();
            assert!(hook.message.contains("hook failure"));
            assert_eq!(hook.message.contains("delivery failure"), failed);
            assert_eq!(files.calls.load(Ordering::SeqCst), 1);
            assert_eq!(driver.delivered, usize::from(!failed));
        }
    }
}
