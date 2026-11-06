//! nf-r-ipc's tables against the request that nf-r-ipc 0.2.2 sent and the answers it decoded,
//! recorded with Nextflow 26.04.6 and answers written as the R launcher writes them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use nextflow_evaluator::number::Number;

use super::table::DATA;
use super::*;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nf_r_ipc")
        .join(name)
}

fn list(items: Vec<Value>) -> Value {
    Value::list(items)
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    Value::map(
        entries
            .into_iter()
            .map(|(key, value)| (Value::string(key), value)),
    )
}

/// A value row as the recording printed it: the fields that are set, in column order.
fn shown(row: &Row) -> String {
    let mut fields = vec![format!(
        "'section': '{}'",
        row.section.clone().unwrap_or_default()
    )];
    let mut add = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            fields.push(format!("'{name}': {value}"));
        }
    };
    let quoted = |text: &Option<String>| text.as_ref().map(|text| format!("'{text}'"));
    add("value_id", row.id.map(|id| id.to_string()));
    add("parent_id", row.parent.map(|id| id.to_string()));
    add("key", quoted(&row.key));
    add("index", row.index.map(|index| index.to_string()));
    add("tag", quoted(&row.tag));
    add("v_string", quoted(&row.text));
    add("v_int64", row.integer.map(|value| value.to_string()));
    add("v_float64", row.float.map(|value| value.to_string()));
    add(
        "v_bool",
        row.boolean
            .map(|value| if value { "True" } else { "False" }.to_owned()),
    );
    format!("{{{}}}", fields.join(", "))
}

#[test]
fn encodes_recorded_request() {
    let integers =
        |values: &[i32]| list(values.iter().map(|value| Value::integer(*value)).collect());
    let arguments = vec![
        ("sample".to_owned(), Value::string("S1")),
        ("n".to_owned(), Value::integer(3)),
        ("x".to_owned(), Value::Number(Number::Double(1.5))),
        ("bd".to_owned(), Value::Number(Number::Double(2.25))),
        ("flag".to_owned(), Value::Boolean(true)),
        ("nothing".to_owned(), Value::Null),
        ("values".to_owned(), integers(&[1, 2, 3])),
        (
            "meta".to_owned(),
            map(vec![
                ("batch", Value::string("B1")),
                (
                    "flags",
                    list(vec![
                        Value::Boolean(true),
                        Value::Boolean(false),
                        Value::Null,
                    ]),
                ),
            ]),
        ),
        (
            "rows".to_owned(),
            list(vec![
                map(vec![
                    ("sample", Value::string("S1")),
                    ("x", Value::integer(1)),
                ]),
                map(vec![
                    ("sample", Value::string("S2")),
                    ("x", Value::integer(2)),
                ]),
            ]),
        ),
        (
            "cols".to_owned(),
            map(vec![
                ("a", integers(&[1, 2])),
                ("b", list(vec![Value::string("x"), Value::string("y")])),
            ]),
        ),
    ];
    let rows = graph::encode(&arguments).expect("encodes");
    let recorded = std::fs::read_to_string(fixture("types-request.txt")).expect("the record");
    let expected: Vec<&str> = (recorded.lines())
        .filter(|line| line.starts_with("{'section': '__nfr_data__'"))
        .collect();
    let written: Vec<String> = rows.iter().map(shown).collect();
    assert_eq!(written, expected);
    let refused = graph::encode(&[("d".to_owned(), Value::Date(0))]).expect_err("a date");
    assert_eq!(
        refused,
        "Unsupported value type in value graph: java.util.Date"
    );
}

#[test]
fn round_trips_table() {
    let rows = graph::encode(&[("a".to_owned(), Value::integer(5_000))]).expect("encodes");
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("request.arrows");
    table::write(&rows, &path).expect("writes");
    assert_eq!(table::read(&path).expect("reads"), rows);
}

#[test]
fn decodes_recorded_answers() {
    let read = |name: &str| table::read(&fixture(name)).expect("reads the stream");
    let value = graph::decode(&read("ok_types.arrows"))
        .expect("decodes")
        .expect("data");
    assert_eq!(
        value.to_string(),
        "[s:S1, i:7, d:2.5, b:true, n:null, na_l:LOGICAL, na_i:INTEGER, na_d:DOUBLE, \
         na_c:CHARACTER, lst:[Infinity, NaN], df:[x:[1.0, DOUBLE]]]"
    );
    let entries = crate::nf_schema::entries(&value);
    assert!(matches!(entries[1].1, Value::Number(Number::Long(7))));
    let columns = graph::decode(&read("ok_columns.arrows"))
        .expect("decodes")
        .expect("data");
    let rows = table_rows(&columns).expect("a table");
    assert_eq!(rows.to_string(), "[[sample:A, v:3.0], [sample:B, v:4.0]]");
    let records = graph::decode(&read("ok_records.arrows"))
        .expect("decodes")
        .expect("data");
    let rows = super::records(&records).expect("records");
    assert_eq!(
        rows.to_string(),
        "[[sample:S1, x:1.0], [sample:S2, x:DOUBLE]]"
    );
    let scalar = graph::decode(&read("ok_scalar.arrows"))
        .expect("decodes")
        .expect("data");
    assert!(table_rows(&scalar).is_none());
    assert_eq!(scalar.java_class_name(), "java.lang.String");
    let two = graph::decode(&read("bad_two_roots.arrows")).expect_err("two roots");
    assert_eq!(two, "Value graph must contain exactly one root, found: 2");
    let error = read("error.arrows");
    let control = error
        .iter()
        .find(|row| row.section.as_deref() == Some(CONTROL))
        .expect("control");
    assert_eq!(control.control[5].as_deref(), Some("error"));
    assert_eq!(control.control[8].as_deref(), Some("boom for diagnostics"));
}

#[test]
fn checks_value_graph() {
    let node = |id: i64, parent: Option<i64>, tag: &str| Row {
        section: Some(DATA.to_owned()),
        id: Some(id),
        parent,
        tag: Some(tag.to_owned()),
        ..Row::default()
    };
    let mut duplicate = vec![
        node(1, None, "list"),
        node(2, Some(1), "null"),
        node(3, Some(1), "null"),
    ];
    duplicate[1].index = Some(0);
    duplicate[2].index = Some(0);
    let found = graph::decode(&duplicate).expect_err("duplicate index");
    assert_eq!(found, "Duplicate list child index 0 for parent 1");
    let mut string = vec![node(1, None, "string")];
    string[0].integer = Some(1);
    let found = graph::decode(&string).expect_err("wrong column");
    assert_eq!(found, "Invalid value columns for string node 1");
    let orphan = vec![node(1, None, "map"), node(2, Some(9), "null")];
    let found = graph::decode(&orphan).expect_err("no parent");
    assert_eq!(found, "Value graph has missing parent for node 2: 9");
    let found = graph::decode(&[node(1, None, "tuple")]).expect_err("unknown tag");
    assert_eq!(found, "Unknown value graph tag: tuple");
}

#[test]
fn helps_with_missing() {
    let helper = |name: &str, arguments: &[Value]| {
        missing::helper(name, arguments)
            .expect("a helper")
            .ok()
            .map(|value| value.to_string())
    };
    let character = missing::value(missing::Kind::Character);
    let double = missing::value(missing::Kind::Double);
    assert_eq!(
        helper("renderValue", std::slice::from_ref(&character)).as_deref(),
        Some("NA<character>")
    );
    assert_eq!(
        helper("renderValue", &[Value::Null]).as_deref(),
        Some("NULL")
    );
    assert_eq!(
        helper("naType", std::slice::from_ref(&double)).as_deref(),
        Some("double")
    );
    assert_eq!(
        helper("coalesce", &[double.clone(), Value::integer(0)]).as_deref(),
        Some("0")
    );
    assert_eq!(helper("isNADouble", &[double]).as_deref(), Some("true"));
    assert_eq!(
        helper("isMissing", &[Value::integer(1)]).as_deref(),
        Some("false")
    );
    let refused = missing::helper("assertNotMissing", &[character, Value::string("sample")]);
    let message = match refused {
        Some(Err(Error::Thrown { exception, .. })) => exception.message.to_string(),
        other => panic!("an error: {other:?}"),
    };
    assert_eq!(message, "Missing value for sample: NA<character>");
    let text = |value: &str| Value::string(value);
    assert_eq!(
        helper("asInstantUtc", &[text("2024-01-02 03:04:05 UTC")]).as_deref(),
        Some("2024-01-02T03:04:05Z")
    );
    assert_eq!(
        helper(
            "asZonedDateTime",
            &[text("2024-01-02 03:04:05 UTC"), text("Europe/Paris")]
        )
        .as_deref(),
        Some("2024-01-02T04:04:05+01:00[Europe/Paris]")
    );
    assert_eq!(
        helper("asLocalDate", &[text("2024-01-02")]).as_deref(),
        Some("2024-01-02")
    );
    assert!(helper("asLocalDate", &[text("nope")]).is_none());
}
