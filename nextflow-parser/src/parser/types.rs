//! Parses type annotations and class names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser};
use crate::ast::{QualifiedName, Type};

impl Parser<'_> {
    /// The `type` rule, like `String`, `List<Path>` or `Map?`.
    pub(super) fn type_annotation(&mut self) -> Parse<Type> {
        Err(self.error_here("Types are not supported yet"))
    }

    /// The `legacyType` rule: a type with optional `[]` pairs, as in `String[] names`.
    pub(super) fn legacy_type(&mut self) -> Parse<Type> {
        Err(self.error_here("Types are not supported yet"))
    }

    /// The `qualifiedClassName` rule, like `java.util.List`.
    pub(super) fn qualified_class_name(&mut self) -> Parse<QualifiedName> {
        Err(self.error_here("Class names are not supported yet"))
    }
}
