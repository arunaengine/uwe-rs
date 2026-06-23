//! Tests of file objects: derived names, secondary file patterns, literals and listings.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn splits_extensions_as_python_does() {
    assert_eq!(split_extension("a.tar.gz"), ("a.tar", ".gz"));
    assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
    assert_eq!(split_extension("a."), ("a", "."));
    assert_eq!(split_extension("noext"), ("noext", ""));
}

#[test]
fn substitutes_secondary_patterns() {
    assert_eq!(substitute("reads.bam", ".bai"), "reads.bam.bai");
    assert_eq!(substitute("reads.bam", "^.bai"), "reads.bai");
    assert_eq!(substitute("ref.fa.gz", "^^.dict"), "ref.dict");
    assert_eq!(substitute("ref", "^.fai"), "ref.fai");
}

#[test]
fn normalizes_files_and_literals() {
    let mut value = Value::parse(
        "[{class: File, location: 'file:///data/a%20b.fastq.gz'}, \
         {class: File, basename: x.txt, contents: hi}]",
    )
    .expect("YAML");
    normalize(&mut value).expect("valid files");
    let files = value.as_list().expect("a list");
    assert_eq!(files[0].text("basename"), Some("a b.fastq.gz"));
    assert_eq!(files[0].text("nameroot"), Some("a b.fastq"));
    assert_eq!(files[0].text("nameext"), Some(".gz"));
    assert!(
        files[1]
            .text("location")
            .is_some_and(|l| l.starts_with("_:"))
    );
    assert_eq!(files[1].text("nameroot"), Some("x"));
}

#[test]
fn resolves_locations_against_the_job() {
    let mut value = Value::parse("{f: {class: File, path: sub/x.txt}}").expect("YAML");
    resolve_locations(&mut value, "file:///work/job.yml");
    let file = value.get("f").expect("f");
    assert_eq!(file.text("location"), Some("file:///work/sub/x.txt"));
    assert!(file.get("path").is_none());
}

#[test]
fn absent_files_count_as_empty_in_a_simulated_run() {
    let mut file = Value::parse("{class: File, location: 'file:///nowhere/x'}").expect("YAML");
    let map = file.as_map_mut().expect("a map");
    let simulated = Access {
        assume_present: true,
    };
    simulated.load_contents(map).expect("contents");
    simulated.add_size(map);
    assert_eq!(map.get("contents"), Some(&Value::string("")));
    assert_eq!(map.get("size"), Some(&Value::Integer(0)));
    assert!(Access::default().read(map, None).is_err());
}
