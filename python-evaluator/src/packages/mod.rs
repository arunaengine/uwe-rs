//! Python packages without a stand-in (decision 19): they run in one Python process, which
//! the embedder starts, for example in a container. The interpreter sends each use of a
//! package object to the process as one request line, and gets one reply line. Objects of the
//! packages stay in the process as handles; plain values come back as values. The design is
//! in `docs/design/snakemake/python-packages.md`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod conda;

pub use conda::conda_packages;

/// The script of the Python process, which answers the requests of the interpreter.
pub const SERVER: &str = include_str!("server.py");
