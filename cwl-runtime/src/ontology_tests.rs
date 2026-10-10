//! Checks RDF parsing, directed subclasses, symmetric equivalents and cyclic graphs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn follows_format_links() {
    let mut relations = Relations::new();
    parse(
        br#"<?xml version="1.0"?>
        <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
                 xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
          <rdf:Description rdf:about="http://example/child">
            <rdfs:subClassOf rdf:resource="http://example/parent"/>
          </rdf:Description>
          <rdf:Description rdf:about="http://example/parent">
            <rdfs:subClassOf rdf:resource="http://example/root"/>
          </rdf:Description>
        </rdf:RDF>"#,
        "http://example/schema.rdf",
        &mut relations,
    )
    .unwrap();
    parse(
        br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
        <alias> owl:equivalentClass <child> .
        <root> owl:equivalentClass <alias> ."#,
        "http://example/schema.ttl",
        &mut relations,
    )
    .unwrap();
    let ontology = Ontology::default();
    ontology.relations.set(Ok(relations)).unwrap();
    for (format, allowed, expected) in [
        ("child", "parent", true),
        ("child", "root", true),
        ("alias", "parent", true),
        ("child", "alias", true),
        ("parent", "unknown", false),
        ("unknown", "parent", false),
    ] {
        assert_eq!(
            ontology
                .matches(
                    &format!("http://example/{format}"),
                    &[format!("http://example/{allowed}")]
                )
                .unwrap(),
            expected
        );
    }
}

#[test]
fn preserves_subclass_direction() {
    let mut relations = Relations::new();
    parse(
        b"<child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <parent> .",
        "http://example/schema.ttl",
        &mut relations,
    )
    .unwrap();
    let ontology = Ontology::default();
    ontology.relations.set(Ok(relations)).unwrap();
    assert!(
        !ontology
            .matches("http://example/parent", &["http://example/child".into()])
            .unwrap()
    );
}

#[test]
fn loads_local_schemas() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("format.ttl");
    std::fs::write(
        &path,
        "<child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <parent> .",
    )
    .unwrap();
    let base = iri::file_url(root.path());
    let ontology = Ontology::new(vec![iri::file_url(&path)], Remote::default());
    assert!(
        ontology
            .matches(&format!("{base}/child"), &[format!("{base}/parent")])
            .unwrap()
    );
    std::fs::remove_file(path).unwrap();
    assert!(
        ontology
            .matches(&format!("{base}/child"), &[format!("{base}/parent")])
            .unwrap()
    );
}

#[test]
fn rejects_bad_schemas() {
    for (base, bytes) in [
        ("http://example/schema.rdf", b"<broken>".as_slice()),
        ("http://example/schema.ttl", b"<incomplete>".as_slice()),
    ] {
        assert!(parse(bytes, base, &mut Relations::new()).is_err());
    }
}

#[test]
fn ignores_url_suffix() {
    for base in [
        "http://example/ontology",
        "http://example/ontology?format=ttl",
    ] {
        let mut relations = Relations::new();
        parse(
            b"<child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <parent> .",
            base,
            &mut relations,
        )
        .unwrap();
        let ontology = Ontology::default();
        ontology.relations.set(Ok(relations)).unwrap();
        assert!(
            ontology
                .matches("http://example/child", &["http://example/parent".into()])
                .unwrap()
        );
    }
}

#[test]
fn scopes_anonymous_classes() {
    let mut relations = Relations::new();
    parse(
        br#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        <child> rdfs:subClassOf _:middle .
        _:middle owl:equivalentClass <parent> ."#,
        "http://example/first",
        &mut relations,
    )
    .unwrap();
    parse(
        br#"_:middle <http://www.w3.org/2000/01/rdf-schema#subClassOf> <unrelated> ."#,
        "http://example/second",
        &mut relations,
    )
    .unwrap();
    let ontology = Ontology::default();
    ontology.relations.set(Ok(relations)).unwrap();
    assert!(
        ontology
            .matches("http://example/child", &["http://example/parent".into()])
            .unwrap()
    );
    assert!(
        !ontology
            .matches("http://example/child", &["http://example/unrelated".into()])
            .unwrap()
    );
}
