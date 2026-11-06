//! nf-parquet against the rows that nf-parquet 0.2.3 read from the files of
//! `fixtures/nf_parquet` with Nextflow 26.04.6, as `recorded.txt` there describes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

/// Each column file of the fixtures with the rows that the plugin printed for it.
const ROWS: &[&str] = &[
    "attrs [id:1, attrs:[k1:1, k2:2]]",
    "attrs [id:2, attrs:null]",
    "attrs [id:3, attrs:[:]]",
    "day [id:1, day:2024-01-02]",
    "day [id:2, day:null]",
    "day [id:3, day:1970-01-01]",
    "dec [id:1, dec:12.34]",
    "dec [id:2, dec:null]",
    "dec [id:3, dec:-0.01]",
    "dec_big [id:1, dec_big:123456789012345678901234567890.12]",
    "dec_big [id:2, dec_big:null]",
    "dict_str [id:1, dict_str:a]",
    "dict_str [id:2, dict_str:b]",
    "f32 [id:1, f32:1.5]",
    "f32 [id:2, f32:null]",
    "f32 [id:3, f32:3.25]",
    "f64 [id:1, f64:0.1]",
    "f64 [id:2, f64:2.0]",
    "f64 [id:3, f64:null]",
    "flag [id:1, flag:true]",
    "flag [id:2, flag:false]",
    "flag [id:3, flag:null]",
    "i16 [id:1, i16:1]",
    "i16 [id:2, i16:2]",
    "i16 [id:3, i16:3]",
    "i32 [id:1, i32:10]",
    "i32 [id:2, i32:null]",
    "i32 [id:3, i32:-3]",
    "i64_unsigned [id:1, i64_unsigned:1]",
    "i64_unsigned [id:2, i64_unsigned:-9223372036854775808]",
    "id [id:1]",
    "id [id:2]",
    "id [id:3]",
    "large_str [id:1, large_str:x]",
    "large_str [id:2, large_str:y]",
    "list_struct [id:1, list_struct:[[a:1]]]",
    "list_struct [id:2, list_struct:[]]",
    "name [id:1, name:alpha]",
    "name [id:2, name:null]",
    "name [id:3, name:gamma]",
    "nested_list [id:1, nested_list:[[1, 2], [3]]]",
    "nested_list [id:2, nested_list:null]",
    "point [id:1, point:[x:1, y:2.5]]",
    "point [id:2, point:null]",
    "point [id:3, point:[x:3, y:null]]",
    "tags [id:1, tags:[a, b]]",
    "tags [id:2, tags:[]]",
    "tags [id:3, tags:null]",
    "time_ms [id:1, time_ms:01:02:03.004]",
    "time_ms [id:2, time_ms:null]",
    "ts_ms [id:1, ts_ms:2024-01-02T03:04:05.123]",
    "ts_ms [id:2, ts_ms:null]",
    "ts_ms [id:3, ts_ms:1970-01-01T00:00]",
    "ts_ns [id:1, ts_ns:2024-01-02T03:04:05.000001]",
    "ts_ns [id:2, ts_ns:null]",
    "ts_us_utc [id:1, ts_us_utc:2024-01-02T03:04:05.123456Z]",
    "ts_us_utc [id:2, ts_us_utc:null]",
    "ts_us_utc [id:3, ts_us_utc:null]",
    "u8 [id:1, u8:1]",
    "u8 [id:2, u8:2]",
    "u8 [id:3, u8:-1]",
];

/// Runs a workflow body with the fixture folder as `dir`; gives the error and the printed text.
fn run(folder: &Path, body: &str) -> (Option<String>, String) {
    let config = "plugins { id 'nf-parquet@0.2.3' }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let include = "include { splitParquet; countParquet; toParquet } from 'plugin/nf-parquet'\n";
    let fixtures = common::fixtures("nf_parquet");
    let script = format!(
        "{include}def dir = '{}'\nworkflow {{\n{body}\n}}\n",
        fixtures.display()
    );
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "parquet");
    (execution.error.map(|error| format!("{error:?}")), printed)
}

#[test]
fn reads_column_types() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let names: Vec<&str> = ROWS
        .iter()
        .filter_map(|row| row.split(' ').next())
        .collect();
    let mut body = String::new();
    for name in names.iter().collect::<std::collections::BTreeSet<_>>() {
        body.push_str(&format!(
            "  channel.fromPath(\"${{dir}}/col_{name}.parquet\").splitParquet().view {{ \"{name} \" + it }}\n"
        ));
    }
    body.push_str(
        "  channel.fromPath(\"${dir}/col_ts_us_utc.parquet\").splitParquet(limit: 1)\n    \
         .view { \"class \" + it.ts_us_utc.getClass().simpleName }\n  \
         channel.fromPath(\"${dir}/col_dec.parquet\").splitParquet(limit: 1)\n    \
         .view { \"class \" + it.dec.getClass().simpleName }\n",
    );
    let (error, printed) = run(directory.path(), &body);
    assert_eq!(error, None, "{printed}");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    let mut expected: Vec<&str> = ROWS.to_vec();
    expected.extend(["class BigDecimal", "class Instant"]);
    expected.sort();
    assert_eq!(lines, expected);
}

#[test]
fn follows_split_options() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let body = "  channel.fromPath(\"${dir}/simple.parquet\").splitParquet(by: 3).view { \"chunk \" + it }
  channel.fromPath(\"${dir}/simple.parquet\").splitParquet(limit: 2, each: { it.name }).view { \"each \" + it }
  channel.of([\"s1\", file(\"${dir}/simple.parquet\")]).splitParquet(elem: 1, limit: 2).view { \"item \" + it }
  channel.of(\"${dir}/simple.parquet\".toString()).splitParquet(limit: 1).view { \"str \" + it }
  channel.fromPath(\"${dir}/empty.parquet\").splitParquet().view { \"empty \" + it }
  channel.fromPath(\"${dir}/{simple,empty}.parquet\").countParquet().view { \"count \" + it }
  channel.fromPath(\"${dir}/simple.parquet\").countParquet(by: 3).view { \"chunks \" + it }";
    let (error, printed) = run(directory.path(), body);
    assert_eq!(error, None, "{printed}");
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "chunk [[id:1, name:n1, score:1.5], [id:2, name:n2, score:3.0], [id:3, name:n3, score:4.5]]",
            "chunk [[id:4, name:n4, score:6.0], [id:5, name:n5, score:7.5], [id:6, name:n6, score:9.0]]",
            "chunk [[id:7, name:n7, score:10.5]]",
            "chunks 3",
            "count 7",
            "each n1",
            "each n2",
            "item [s1, [id:1, name:n1, score:1.5]]",
            "item [s1, [id:2, name:n2, score:3.0]]",
            "str [id:1, name:n1, score:1.5]",
        ]
    );
}

/// The plugin checks the record class of each file it reads, so an empty channel passes; a
/// class of the script is not a Java record either.
#[test]
fn checks_record_classes() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let body = "  channel.empty().countParquet(record: String).view { \"count \" + it }";
    let (error, printed) = run(directory.path(), body);
    assert_eq!((error, printed.trim()), (None, "count 0"));
    let script = format!(
        "include {{ splitParquet }} from 'plugin/nf-parquet'\nclass C {{ long id }}\n\
         workflow {{ channel.fromPath('{}/simple.parquet').splitParquet(record: C).view() }}\n",
        common::fixtures("nf_parquet").display()
    );
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(directory.path(), "parquet");
    let error = format!("{:?}", execution.error);
    assert!(
        error.contains("C is not a Java Record"),
        "{error}\n{printed}"
    );
}

#[test]
fn reports_errors() {
    let cases = [
        (
            "channel.fromPath(\"${dir}/simple.parquet\").splitParquet(foo: 3).view()",
            "Unknown argument 'foo' for operator 'split' -- Possible arguments: each, by, into, \
             autoClose, limit, elem, record, file",
        ),
        (
            "channel.fromPath(\"${dir}/col_bin.parquet\").splitParquet().view()",
            "BINARY deserialization not supported",
        ),
        (
            "channel.fromPath(\"${dir}/col_fixed_bin.parquet\").splitParquet().view()",
            "FIXED_LEN_BYTE_ARRAY deserialization not supported",
        ),
        (
            "channel.fromPath(\"${dir}/recorded.txt\").splitParquet().view()",
            "recorded.txt is not a Parquet file",
        ),
        (
            "channel.fromPath(\"${dir}/simple.parquet\").splitParquet(by: 3, file: true).view()",
            "A Record.class is required. Class provided null",
        ),
        (
            "channel.of(1).toParquet(\"${dir}/out.parquet\", [record: 'x', foo: 1]).view()",
            "Unknow foo arguments. Posible values [record, by]",
        ),
        (
            "channel.of(1).toParquet(\"${dir}/out.parquet\", [by: 2]).view()",
            "A Record.class is required. Class provided null",
        ),
        // uwe has no Java records, so every class fails as one that is not a record does.
        (
            "channel.of(1).toParquet(\"${dir}/out.parquet\", [record: String]).view()",
            "java.lang.String must be a java Record",
        ),
        (
            "channel.fromPath(\"${dir}/simple.parquet\").splitParquet(record: String).view()",
            "java.lang.String is not a Java Record",
        ),
        (
            "channel.fromPath(\"${dir}/simple.parquet\").countParquet(record: String).view()",
            "java.lang.String is not a Java Record",
        ),
        (
            "channel.fromPath(\"${dir}/simple.parquet\").splitParquet(file: true, record: String)",
            "java.lang.String is not a Java Record",
        ),
        (
            "channel.fromPath(\"${dir}/simple.parquet\").splitParquet(record: 'Proj').view()",
            "Value 'Proj' cannot be used in in parameter 'record' for operator 'split' -- Value \
             don't match: class java.lang.Class",
        ),
        (
            "channel.of(5).splitParquet().view()",
            "Object of class 'java.lang.Integer' does not support 'splitter' methods",
        ),
    ];
    for (body, message) in cases {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let (error, printed) = run(directory.path(), body);
        let error = error.unwrap_or_default();
        assert!(error.contains(message), "{body}\n{error}\n{printed}");
    }
}
