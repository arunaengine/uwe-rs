//! Selects fixture completion orders to detect dependence on task finish order.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Order, RunOptions};

/// The run options that `CWL_TEST_WORKFLOWS_ORDER` asks for.
pub fn run_options() -> RunOptions {
    let asked = std::env::var("CWL_TEST_WORKFLOWS_ORDER").unwrap_or_default();
    let order = match asked.split_once(':') {
        Some(("shuffled", seed)) => Order::Shuffled {
            seed: seed.parse().expect("a number as the seed"),
        },
        _ if asked == "reversed" => Order::Reversed,
        _ => return RunOptions::default(),
    };
    RunOptions {
        max_pending: 8,
        order,
    }
}
