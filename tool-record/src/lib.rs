//! The CWL CommandLineTool that the recorder makes of a shell command, and its YAML text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

mod tool;

pub use tool::{Argument, Binding, Input, Kind, Output, Tool};
