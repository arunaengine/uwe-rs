//! R's typed missing values (`NA` of logical, integer, double and character) as script values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::value::{Native, Value};

/// The type of an R `NA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Logical,
    Integer,
    Double,
    Character,
}

impl Kind {
    /// The tag of the value graph, like `na_double`.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Kind::Logical => "na_logical",
            Kind::Integer => "na_integer",
            Kind::Double => "na_double",
            Kind::Character => "na_character",
        }
    }

    pub(crate) fn from_tag(tag: &str) -> Option<Kind> {
        [Kind::Logical, Kind::Integer, Kind::Double, Kind::Character]
            .into_iter()
            .find(|kind| kind.tag() == tag)
    }

    /// The R type name, like `double`.
    fn name(self) -> &'static str {
        &self.tag()[3..]
    }
}

/// An `NA` value, which prints as its kind, like `DOUBLE`.
#[derive(Debug)]
struct Missing(Kind);

impl Native for Missing {
    fn plugin(&self) -> &'static str {
        "nf-r-ipc"
    }

    fn text(&self) -> String {
        self.0.name().to_uppercase()
    }

    fn key(&self) -> Option<String> {
        Some(self.text())
    }
}

/// The `NA` value of a kind.
pub(crate) fn value(kind: Kind) -> Value {
    crate::object("NAValue", Missing(kind))
}

/// The kind of an `NA` value, `None` for any other value.
pub(crate) fn kind(value: &Value) -> Option<Kind> {
    crate::native::<Missing>(value).map(|missing| missing.0)
}
