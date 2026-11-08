//! Lowers a WDL workflow into a wir graph.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use wdl_runtime::document::Unit;
use wir::Graph;

use crate::Lowerer;

impl Lowerer {
    /// The graph `name` of a workflow, or the first thing in it that has no lowering yet.
    pub(crate) fn workflow(&mut self, _unit: &Unit, _name: &str) -> Result<Graph, String> {
        Err("workflows have no lowering yet".into())
    }
}
