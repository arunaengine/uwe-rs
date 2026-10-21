//! Keeps the helper's synchronous executor runtime outside a caller's Tokio runtime.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::ops::{Deref, DerefMut};

use executor::process::ContainerProcess;

use crate::javascript::helper::Failure;

pub(crate) fn run<T: Send>(operation: impl FnOnce() -> T + Send) -> Result<T, Failure> {
    std::thread::scope(|scope| scope.spawn(operation).join())
        .map_err(|_| Failure::Boundary("JavaScript helper operation thread panicked".into()))
}

pub(crate) struct HelperProcess(Option<ContainerProcess>);

impl HelperProcess {
    pub(crate) fn new(process: ContainerProcess) -> Self {
        Self(Some(process))
    }
}

impl Deref for HelperProcess {
    type Target = ContainerProcess;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("helper process is present")
    }
}

impl DerefMut for HelperProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("helper process is present")
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if let Some(process) = self.0.take() {
            let _ = run(|| drop(process));
        }
    }
}
