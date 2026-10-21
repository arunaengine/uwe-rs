//! Checks Java and Groovy method names against groovy_methods.txt recorded by the oracle.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const RECORDED: &str = include_str!("groovy_methods.txt");

/// Whether Groovy has a method of this name for a value of the Java class; `None` for a class
/// that the record does not cover.
pub(in crate::interpreter) fn has_method(java_class: &str, name: &str) -> Option<bool> {
    static METHODS: OnceLock<HashMap<&'static str, HashSet<&'static str>>> = OnceLock::new();
    let methods = METHODS.get_or_init(|| {
        RECORDED
            .lines()
            .filter_map(|line| line.split_once(": "))
            .map(|(class, names)| (class, names.split(' ').collect()))
            .collect()
    });
    methods.get(java_class).map(|names| names.contains(name))
}
