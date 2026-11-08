//! Lowers a call of a WDL task; every task stays a native op yet.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_runtime::document::Unit;
use wir::Op;

use crate::Lowerer;

impl Lowerer {
    /// The op at `path` that runs a call of a task.
    pub(crate) fn task(&mut self, unit: &Unit, path: &str) -> Result<Op, String> {
        self.native(unit, path, "tasks have no lowering yet".into())
    }
}
