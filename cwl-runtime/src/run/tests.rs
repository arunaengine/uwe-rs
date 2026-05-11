//! Tests of the names of the jobs of a run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn names_scatter_elements_by_index() {
    let names = Names::default()
        .child("align")
        .child("sort")
        .indexed(&[1, 2]);
    assert_eq!(names.display, "align/sort[1,2]");
    assert_eq!(names.step, "align/sort");
}
