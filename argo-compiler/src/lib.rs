//! Compiles Argo workflows into wir: steps and DAG templates into graphs, and their steps and
//! tasks into Argo dialect tasks, or the whole workflow into one native op where it needs more.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod depends;
pub mod tags;
