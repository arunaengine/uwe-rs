//! nf-fgbio 1.0.0: `readStructure` parses an fgbio read structure, like `8M+T`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Object, State, Value};

pub(crate) fn call(name: &str, arguments: Vec<Value>) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("readStructure", [text]) => read_structure(&text.to_string()),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-fgbio` with these arguments"),
            span: Default::default(),
        }),
    }
}

/// A `ReadStructure`, which prints as its segments: a length, or `+` for the rest of the read,
/// then the kind `T`, `B`, `M` or `S`. Case and blanks do not matter.
fn read_structure(text: &str) -> Result<Value, Error> {
    let tidied: String = text
        .to_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let characters: Vec<char> = tidied.chars().collect();
    // Each segment: whether it has a length, and whether that length is zero.
    let mut segments: Vec<(bool, bool)> = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let start = index;
        let length = match characters[index] {
            '+' => {
                index += 1;
                None
            }
            c if c.is_ascii_digit() => {
                while index < characters.len() && characters[index].is_ascii_digit() {
                    index += 1;
                }
                Some(characters[start..index].iter().all(|c| *c == '0'))
            }
            _ => {
                return Err(invalid(
                    "Read structure missing length information",
                    &characters,
                    start,
                    start + 1,
                ));
            }
        };
        if index == characters.len() {
            return Err(invalid(
                "Read structure with invalid segment",
                &characters,
                start,
                index,
            ));
        }
        index += 1;
        if !matches!(characters[index - 1], 'T' | 'B' | 'M' | 'S') {
            return Err(invalid(
                "Read structure segment had unknown type",
                &characters,
                start,
                index,
            ));
        }
        segments.push((length.is_some(), length == Some(true)));
    }
    if segments.is_empty() {
        return Err(failure(
            "requirement failed: Can't create a read structure with zero segments.",
        ));
    }
    if segments[..segments.len() - 1]
        .iter()
        .any(|(fixed, _)| !fixed)
    {
        return Err(failure(&format!(
            "requirement failed: Variable length (+) can only be used in the last segment: {tidied}"
        )));
    }
    if segments.iter().any(|(_, zero)| *zero) {
        return Err(failure(&format!(
            "Read structure contained zero length segments: {tidied}"
        )));
    }
    Ok(Value::Object(Rc::new(RefCell::new(Object {
        class: "com.fulcrumgenomics.util.ReadStructure",
        state: State::Text(tidied.into()),
    }))))
}

/// A parse error that marks the part in brackets, like `10B[20C]+T`.
fn invalid(message: &str, characters: &[char], start: usize, end: usize) -> Error {
    let part = |range: std::ops::Range<usize>| characters[range].iter().collect::<String>();
    failure(&format!(
        "{message}: {}[{}]{}",
        part(0..start),
        part(start..end),
        part(end..characters.len())
    ))
}

/// The `IllegalArgumentException` of fgbio, which the call by reflection wraps.
fn failure(message: &str) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}
