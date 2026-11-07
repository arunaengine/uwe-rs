//! The WDL standard library: the functions that expressions call.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::expression::{Environment, Error};
use crate::value::Value;

/// The value of a call of a standard library function.
pub fn call(
    name: &str,
    _arguments: Vec<Value>,
    _env: &mut dyn Environment,
) -> Result<Value, Error> {
    Err(Error::Unsupported(format!(
        "the function `{name}` is not supported"
    )))
}
