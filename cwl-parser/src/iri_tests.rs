//! Tests IRI parsing, reference resolution and the conversion between paths and file URLs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use crate::iri::{Parts, file_path, file_url, has_scheme, join, split_fragment};

#[test]
fn resolves_rfc_examples() {
    let base = "http://a/b/c/d;p?q";
    let cases = [
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g"),
        ("?y", "http://a/b/c/d;p?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("", "http://a/b/c/d;p?q"),
        (".", "http://a/b/c/"),
        ("..", "http://a/b/"),
        ("../g", "http://a/b/g"),
        ("../../../g", "http://a/g"),
        ("g;x=1/../y", "http://a/b/c/y"),
        ("https:g", "https:g"),
    ];
    for (reference, expected) in cases {
        assert_eq!(join(base, reference), expected, "reference {reference}");
    }
}

#[test]
fn joins_file_references() {
    let base = "file:///work/flows/main.cwl";
    assert_eq!(
        join(base, "tools/cat.cwl"),
        "file:///work/flows/tools/cat.cwl"
    );
    assert_eq!(
        join(base, "../types.yml#Sample"),
        "file:///work/types.yml#Sample"
    );
    assert_eq!(
        join(base, "#main/step"),
        "file:///work/flows/main.cwl#main/step"
    );
    assert_eq!(
        join("file:///work/main.cwl#main", "#other"),
        "file:///work/main.cwl#other"
    );
    assert_eq!(
        join(base, "http://example.org/x.cwl"),
        "http://example.org/x.cwl"
    );
}

#[test]
fn splits_parts() {
    let parts = Parts::parse("https://host:8080/a/b.cwl?x=1#main/step");
    assert_eq!(parts.scheme, Some("https"));
    assert_eq!(parts.authority, Some("host:8080"));
    assert_eq!(parts.path, "/a/b.cwl");
    assert_eq!(parts.query, Some("x=1"));
    assert_eq!(parts.fragment, Some("main/step"));
    assert_eq!(split_fragment("a.cwl#main"), ("a.cwl", Some("main")));
    assert_eq!(split_fragment("a.cwl"), ("a.cwl", None));
}

#[test]
fn recognizes_schemes() {
    assert!(has_scheme("file:///a"));
    assert!(has_scheme("edam:format_1930"));
    assert!(!has_scheme("tools/cat.cwl"));
    assert!(!has_scheme("a b:c"));
    assert!(!has_scheme("dir/x:y.cwl"));
    assert!(!has_scheme("#main"));
}

#[test]
fn converts_file_paths() {
    let url = file_url(Path::new("/work/my tools/cat#1.cwl"));
    assert_eq!(url, "file:///work/my%20tools/cat%231.cwl");
    assert_eq!(
        file_path(&url),
        Some(PathBuf::from("/work/my tools/cat#1.cwl"))
    );
    assert_eq!(
        file_path("file:///work/a.cwl#main"),
        Some(PathBuf::from("/work/a.cwl"))
    );
    assert_eq!(file_path("http://example.org/a.cwl"), None);
}
