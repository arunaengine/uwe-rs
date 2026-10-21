//! Tests the expansion rules of Schema Salad and the annotation table of CWL fields.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::salad::context::{
    Expansion, MapForm, Namespaces, is_term, map_form, reference_scope, term_of,
};

const BASE: &str = "file:///work/flow.cwl";

fn declared() -> Namespaces {
    Namespaces::default().extend(vec![
        ("edam".to_owned(), "http://edamontology.org/".to_owned()),
        (
            "cwltool".to_owned(),
            "http://commonwl.org/cwltool#".to_owned(),
        ),
    ])
}

#[test]
fn expands_declared_prefixes() {
    let namespaces = declared();
    let expand = |text| namespaces.expand(text, BASE, Expansion::LINK);
    assert_eq!(
        expand("edam:format_1930"),
        "http://edamontology.org/format_1930"
    );
    assert_eq!(expand("unknown:thing"), "unknown:thing");
    assert_eq!(expand("cwl:File"), "https://w3id.org/cwl/cwl#File");
}

#[test]
fn scopes_identifiers() {
    let namespaces = Namespaces::default();
    let expand = |text, base| namespaces.expand(text, base, Expansion::IDENTIFIER);
    assert_eq!(expand("main", BASE), "file:///work/flow.cwl#main");
    assert_eq!(
        expand("step", "file:///work/flow.cwl#main"),
        "file:///work/flow.cwl#main/step"
    );
    assert_eq!(
        expand("#other", "file:///work/flow.cwl#main"),
        "file:///work/flow.cwl#other"
    );
    assert_eq!(expand("file:///work/x.cwl#y", BASE), "file:///work/x.cwl#y");
}

#[test]
fn keeps_vocabulary_terms() {
    let namespaces = declared();
    let expand = |field, text| namespaces.expand(text, BASE, Expansion::field(field));
    assert_eq!(expand("type", "File"), "File");
    assert_eq!(expand("type", "xsd:string"), "string");
    assert_eq!(expand("type", "sld:null"), "null");
    assert_eq!(expand("type", "Sample"), "Sample");
    assert_eq!(expand("type", "#Sample"), "file:///work/flow.cwl#Sample");
    assert_eq!(
        expand("type", "types.yml#Sample"),
        "file:///work/types.yml#Sample"
    );
    assert_eq!(
        expand("class", "cwl:DockerRequirement"),
        "DockerRequirement"
    );
    assert_eq!(
        expand("class", "cwltool:Secrets"),
        "http://commonwl.org/cwltool#Secrets"
    );
    assert_eq!(expand("cwlVersion", "cwl:v1.0"), "v1.0");
    assert_eq!(expand("scatterMethod", "dotproduct"), "dotproduct");
    assert!(is_term("the_only_non_null"));
    assert_eq!(
        term_of("http://www.w3.org/2001/XMLSchema#long"),
        Some("long")
    );
}

#[test]
fn joins_links() {
    let namespaces = Namespaces::default();
    let expand = |field, text| namespaces.expand(text, BASE, Expansion::field(field));
    assert_eq!(expand("run", "tools/cat.cwl"), "file:///work/tools/cat.cwl");
    assert_eq!(expand("location", "$(inputs.x)"), "$(inputs.x)");
    assert_eq!(expand("source", "step/out"), "step/out");
    assert_eq!(expand("source", "#main/x"), "file:///work/flow.cwl#main/x");
}

#[test]
fn describes_identifier_maps() {
    let inputs = MapForm {
        subject: "id",
        predicate: Some("type"),
    };
    assert_eq!(map_form("inputs"), Some(inputs));
    assert_eq!(map_form("steps").and_then(|form| form.predicate), None);
    assert_eq!(map_form("doc"), None);
    assert_eq!(reference_scope("source"), Some(2));
    assert_eq!(reference_scope("outputSource"), Some(0));
    assert_eq!(reference_scope("run"), None);
}
