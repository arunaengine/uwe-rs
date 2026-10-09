//! Runs CWL tools and workflows through the execution graph driver.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod binding;
mod driver;
pub mod expression;
pub mod files;
mod image;
mod inputs;
mod javascript;
mod ontology;
mod placeholder;
mod results;
mod run;
mod schema;
mod scope;
pub mod tool;
pub mod types;
pub mod value;

pub use driver::{CwlDriver, driver, driver_with, driver_with_options, process_name};
pub use expression::{Evaluator, Variables};
pub use javascript::helper::{JavaScriptSettings, javascript_worker_main};
pub use tool::Options;
pub use value::{Map, Value};
