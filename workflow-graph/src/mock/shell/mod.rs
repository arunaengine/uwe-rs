//! A small simulation of the shell scripts of tasks, for the mock executors: which files a
//! script makes and what text it writes into them. It follows the commands that stub scripts
//! use to make their files, like `touch`, `mkdir`, `echo`, `printf`, `cat` with a here
//! document, `cp`, `mv`, `gzip` and `for` loops over words. Anything else, like a tool or an
//! `if`, makes the simulation incomplete: the script may make more files than it tells.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod lexer;
pub mod words;
