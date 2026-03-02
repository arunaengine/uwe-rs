//! Lowering: resolves the names of a syntax tree as Nextflow's compiler does, and reports the
//! same load-time errors, like "`x` is not defined".
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod names;
mod scope;
