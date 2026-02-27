//! Java exceptions and errors, as scripts see and catch them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::number::ArithmeticError;

/// A thrown Java exception or error: its simple class name and its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    pub class: &'static str,
    pub message: String,
}

impl Exception {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    pub fn illegal_argument(message: impl Into<String>) -> Self {
        Self::new("IllegalArgumentException", message)
    }

    /// Java's `NumberFormatException` for text that is not a number.
    pub fn number_format(text: &str) -> Self {
        Self::new(
            "NumberFormatException",
            format!("For input string: \"{text}\""),
        )
    }

    /// A failed Groovy `assert`. It is an `Error`, which `catch (e)` does not catch.
    pub fn assertion(message: impl Into<String>) -> Self {
        Self::new("AssertionError", message)
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}

impl std::error::Error for Exception {}

impl From<ArithmeticError> for Exception {
    fn from(error: ArithmeticError) -> Self {
        match error {
            ArithmeticError::Arithmetic(message) => Self::new("ArithmeticException", message),
            ArithmeticError::Unsupported(message) => {
                Self::new("UnsupportedOperationException", message)
            }
        }
    }
}
