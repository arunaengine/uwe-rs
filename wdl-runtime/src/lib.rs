//! Runs WDL documents through the execution graph driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod document;
mod driver;
pub mod expression;
pub mod library;
mod schema;
pub mod task;
pub mod value;
mod workflow;

pub use driver::{Options, WdlDriver, driver, driver_with};
pub use value::Value;
