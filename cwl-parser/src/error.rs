//! The error type of the loader: a message and the location it refers to.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::node::Location;

/// A problem found while reading, preprocessing or checking a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub location: Location,
    pub message: String,
}

impl Error {
    pub fn new(location: &Location, message: impl Into<String>) -> Self {
        Self {
            location: location.clone(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.location, self.message)
    }
}

impl std::error::Error for Error {}
