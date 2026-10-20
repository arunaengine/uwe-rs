//! Tests for the checks of tag references against a scope.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Globals, Scope};

#[test]
fn checks_known_prefixes() {
    let mut scope = Scope::default();
    scope.insert("inputs.parameters.message");
    let globals = Globals::default();
    assert_eq!(
        scope.resolve("{{inputs.parameters.message}}", &globals),
        Ok(())
    );
    assert_eq!(
        scope.resolve("echo {{inputs.parameters.mesage}}", &globals),
        Err("failed to resolve {{inputs.parameters.mesage}}".into())
    );
    // Unknown prefixes, expression tags and late variables stay unchecked, as in Argo.
    let unchecked = "{{custom.x}} {{=inputs.parameters.nope}} {{outputs.x}} {{item}} {{retries}}";
    assert_eq!(scope.resolve(unchecked, &globals), Ok(()));
}

#[test]
fn items_widen_scope() {
    let globals = Globals::default();
    let mut scope = Scope::default();
    assert!(scope.resolve("{{item.name}}", &globals).is_err());
    scope.add_items(&[], "{{inputs.parameters.list}}", false);
    assert_eq!(scope.resolve("{{item.name}}", &globals), Ok(()));
}
