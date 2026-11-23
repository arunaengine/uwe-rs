//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "arithmetic.rs"]
mod arithmetic;
#[path = "classes.rs"]
mod classes;
#[path = "collections.rs"]
mod collections;
#[path = "compare.rs"]
mod compare;
#[path = "config.rs"]
mod config;
#[path = "dataflow.rs"]
mod dataflow;
#[path = "decimal.rs"]
mod decimal;
#[path = "format.rs"]
mod format;
#[path = "interpret.rs"]
mod interpret;
#[path = "json.rs"]
mod json;
#[path = "lower.rs"]
mod lower;
#[path = "lower_legacy.rs"]
mod lower_legacy;
#[path = "lower_process.rs"]
mod lower_process;
#[path = "lower_workflow.rs"]
mod lower_workflow;
#[path = "maps.rs"]
mod maps;
#[path = "matching.rs"]
mod matching;
#[path = "number.rs"]
mod number;
#[path = "numbers.rs"]
mod numbers;
#[path = "operators.rs"]
mod operators;
#[path = "org_json.rs"]
mod org_json;
#[path = "paths.rs"]
mod paths;
#[path = "ranges.rs"]
mod ranges;
#[path = "regex.rs"]
mod regex;
#[path = "strings.rs"]
mod strings;
#[path = "task.rs"]
mod task;
#[path = "test_workflows.rs"]
mod test_workflows;
#[path = "units.rs"]
mod units;
#[path = "value.rs"]
mod value;
