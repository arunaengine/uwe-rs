//! Turns a shell command into a CWL CommandLineTool, and writes the tool as YAML.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

mod command;
mod tool;

pub use command::{name, parse};
pub use tool::{Argument, Binding, Input, Kind, Output, Tool};
