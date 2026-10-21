//! Schema Salad preprocessing of CWL documents: imports, identifiers, links and the type DSL.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod context;
mod import;
mod links;
mod resolve;

pub use import::find_object;
pub use links::check as check_links;
pub use resolve::Resolver;
