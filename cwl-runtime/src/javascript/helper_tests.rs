//! Bounds are checked before starting a helper or parsing its response.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;
use crate::expression::{Evaluator, Variables};
use cwl_parser::model::Expression;

#[test]
fn preserves_javascript_literals() {
    let mut value = Value::parse("{z: [1, 1.25, 'text'], a: null}").expect("value");
    let Value::Map(map) = &mut value else {
        panic!("map")
    };
    map.insert("line\u{2028}".into(), Value::String("\u{2029}\n\"".into()));
    map.insert("nan".into(), Value::Float(f64::NAN));
    map.insert("infinity".into(), Value::Float(f64::INFINITY));
    let mut frame = Capped {
        bytes: Vec::new(),
        cancel: CancellationToken::new(),
        deadline: Instant::now() + TIME_LIMIT,
    };
    write_value(&mut frame, &value, 0).expect("streams");
    assert_eq!(
        String::from_utf8(frame.bytes).expect("UTF8"),
        value.to_javascript()
    );
}

#[test]
fn cancellation_preserves_references() {
    let evaluator = Evaluator::with_javascript(Vec::new());
    evaluator.settings.cancel.cancel();
    let inputs = Value::parse("{value: 42}").expect("inputs");
    let vars = Variables {
        inputs: &inputs,
        this: &Value::Null,
        runtime: &Value::Null,
    };
    let expression = Expression::parse("${HOME}", false).expect("expression");
    let error = evaluator
        .evaluate(&expression, vars)
        .expect_err("cancellation must propagate");
    assert!(boundary(&error) && error.contains("cancelled"), "{error}");
    let reference = Expression::parse("$(inputs.value)", false).expect("reference");
    assert_eq!(evaluator.evaluate(&reference, vars), Ok(Value::Integer(42)));
}

#[test]
fn rebinds_cancelled_helper() {
    let mut settings =
        JavaScriptSettings::new("configured-engine".into(), CancellationToken::new());
    settings.image = "configured-image".into();
    let previous = settings.helper.clone();
    let cancel = CancellationToken::new();
    settings.bind_cancel(cancel.clone());
    assert!(!Arc::ptr_eq(&previous, &settings.helper));
    assert_eq!(settings.engine, "configured-engine");
    assert_eq!(settings.image, "configured-image");
    cancel.cancel();
    assert!(settings.cancel.is_cancelled());
}

#[test]
fn bounds_helper_requests() {
    for value in [Value::String("x".repeat(FRAME_LIMIT)), {
        let mut value = Value::Null;
        for _ in 0..NESTING_LIMIT + 2 {
            value = Value::List(vec![value]);
        }
        value
    }] {
        let evaluator = Evaluator::with_javascript(Vec::new());
        let expression = Expression::parse("${return self;}", false).expect("expression");
        let error = evaluator
            .evaluate(
                &expression,
                Variables {
                    inputs: &Value::Null,
                    this: &value,
                    runtime: &Value::Null,
                },
            )
            .expect_err("request limit must propagate");
        assert!(boundary(&error), "{error}");
        assert!(
            error.contains("frame exceeded") || error.contains("nesting exceeds"),
            "{error}"
        );
        assert!(
            evaluator
                .settings
                .helper
                .lock()
                .expect("helper lock")
                .is_none()
        );
    }
}

#[test]
fn bounds_response_parsing() {
    let deep = format!(
        "{}null{}",
        "[".repeat(NESTING_LIMIT + 1),
        "]".repeat(NESTING_LIMIT + 1)
    );
    assert!(
        check_frame(&deep, NESTING_LIMIT)
            .expect_err("depth")
            .contains("nesting")
    );
    assert!(
        check_frame(&" ".repeat(FRAME_LIMIT + 1), NESTING_LIMIT)
            .expect_err("size")
            .contains("frame")
    );
    assert!(check_frame(r#"{"value":"[{\"quoted\"}]"}"#, NESTING_LIMIT).is_ok());
}
