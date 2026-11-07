//! wir, the workflow intermediate representation: one typed graph format for workflow languages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod flow;
mod types;

pub use flow::Flow;
pub use types::{
    ArrayType, Field, FileType, KeyType, MapType, SecondaryFile, Type, TypeDef, resolve,
};
