//! Tests of input objects: placeholders of the top process, and format checks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::sync::Arc;

use cwl_parser::Loader;
use cwl_parser::source::{Memory, Remote};

use super::*;
use crate::expression::Evaluator;

const BASE: &str = "file:///in/";

fn process(text: &str) -> Arc<Process> {
    let mut memory = Memory::default();
    memory.insert("file:///work/tool.cwl", text);
    let document = Loader::new(memory).load("file:///work/tool.cwl");
    document.unwrap_or_else(|error| panic!("{error}")).process
}

/// The setting of the process a run on placeholder inputs starts with.
fn setting<'a>(types: &'a Types, evaluator: &'a Evaluator, assumed: &'a [String]) -> Setting<'a> {
    static NONE: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
    // Without schemas the folder of downloads stays unused; a default one would outlive the tests.
    static ONTOLOGY: std::sync::LazyLock<Ontology> = std::sync::LazyLock::new(|| {
        let remote = Remote::new(&storage::Storage::default(), std::env::temp_dir());
        Ontology::new(Vec::new(), remote)
    });
    Setting {
        types,
        evaluator,
        access: Access {
            assume_present: true,
            ..Access::default()
        },
        load_listing: None,
        discover: true,
        placeholders: Some(BASE),
        toplevel: true,
        fill_optional: false,
        ontologies: &ONTOLOGY,
        assumed,
        placeholder_strings: &NONE,
    }
}

#[test]
fn supplies_requested_placeholders() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  extra: File?\n  flag: boolean\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let setting = Setting {
        fill_optional: true,
        ..setting(&types, &evaluator, &assumed)
    };
    let job = prepare(&tool, Map::new(), &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("extra").and_then(|file| file.text("location")),
        Some("file:///in/extra.txt")
    );
    assert_eq!(job.get("flag"), Some(&Value::Bool(true)));
    let plain = Setting {
        fill_optional: false,
        ..setting
    };
    let job = prepare(&tool, Map::new(), &plain).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(job.get("extra"), Some(&Value::Null));
    assert_eq!(job.get("flag"), Some(&Value::Bool(false)));
}

#[test]
fn loads_array_contents() {
    let folder = tempfile::tempdir().expect("folder");
    let file = folder.path().join("input.txt");
    std::fs::write(&file, "array contents").expect("input file");
    for version in ["v1.1", "v1.2"] {
        let tool = process(&format!(
            "cwlVersion: {version}\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
             inputs: {{files: {{type: 'File[]', loadContents: true}}}}\n"
        ));
        let (types, evaluator) = (Types::default(), Evaluator::default());
        let settings = Setting {
            placeholders: None,
            access: Access::default(),
            ..setting(&types, &evaluator, &[])
        };
        let Value::Map(job) = Value::parse(&serde_json::json!({"files": [{"class": "File", "location": cwl_parser::iri::file_url(&file)}]}).to_string()).expect("job") else { panic!("map") };
        let job = prepare(&tool, job, &settings).expect("prepared inputs");
        assert_eq!(
            job["files"].as_list().unwrap()[0].text("contents"),
            Some("array contents")
        );
    }
}

#[test]
fn replaces_broken_defaults() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  given:\n    type: File\n    default: {class: File}\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let setting = setting(&types, &evaluator, &assumed);
    let job = prepare(&tool, Map::new(), &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("given").and_then(|file| file.text("location")),
        Some("file:///in/given.txt")
    );
    // Inside a workflow such a default is an error, as in cwltool.
    let inner = Setting {
        toplevel: false,
        ..setting
    };
    let job = prepare(&tool, Map::new(), &inner);
    assert_eq!(
        job,
        Err("the input `given`: a File needs a location, a path or contents".to_owned())
    );
}

#[test]
fn names_invalid_types() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
         inputs:\n  database: string\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let setting = Setting {
        placeholders: None,
        ..setting(&types, &evaluator, &[])
    };
    let mut job = Map::new();
    let file = Value::parse("{class: File, location: 'file:///db/uniref90.dmnd'}").expect("YAML");
    job.insert("database".into(), file);
    assert_eq!(
        prepare(&tool, job, &setting),
        Err(
            "the input `database` is of type string, but its value is a File \
             `uniref90.dmnd`"
                .to_owned()
        )
    );
}

const FORMATS: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\n\
    outputs: []\ninputs:\n  regular: {type: File, format: 'http://example.com/format1'}\n  \
    record:\n    type:\n      type: record\n      fields:\n        \
    f2: {type: 'File[]', format: 'http://example.com/format2'}\n";

/// The input object of the format tool with these two formats, which `None` leaves out.
fn formats(regular: Option<&str>, item: &str) -> Map {
    let mut file = "{class: File, location: 'file:///d'".to_owned();
    if let Some(format) = regular {
        file.push_str(&format!(", format: '{format}'"));
    }
    file.push('}');
    let job = format!(
        "{{regular: {file}, record: {{f2: [{{class: File, location: 'file:///b', \
         format: '{item}'}}]}}}}"
    );
    match Value::parse(&job) {
        Ok(Value::Map(map)) => map,
        other => panic!("{other:?}"),
    }
}

#[test]
fn checks_cwl_formats() {
    let tool = process(FORMATS);
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let setting = setting(&types, &evaluator, &assumed);
    let one = "http://example.com/format1";
    let two = "http://example.com/format2";
    let other = "http://example.com/formatZ";
    assert!(prepare(&tool, formats(Some(one), two), &setting).is_ok());
    let wrong = prepare(&tool, formats(Some(other), two), &setting);
    assert!(wrong.is_err_and(|error| error.contains("formatZ")));
    let wrong_item = prepare(&tool, formats(Some(one), other), &setting);
    assert!(wrong_item.is_err_and(|error| error.contains("formatZ")));
    let missing = prepare(&tool, formats(None, two), &setting);
    assert!(missing.is_err_and(|error| error.contains("no format")));
    // A placeholder file without a format takes the declared one.
    let mut job = formats(None, two);
    if let Some(Value::Map(file)) = job.get_mut("regular") {
        file.insert("location".into(), Value::string("file:///in/regular.txt"));
    }
    let job = prepare(&tool, job, &setting).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        job.get("regular").and_then(|file| file.text("format")),
        Some(one)
    );
}

const INDEXED: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
    outputs: []\ninputs:\n  reference: {type: File, secondaryFiles: [.fai]}\n";

#[test]
fn requires_secondary_files() {
    let tool = process(INDEXED);
    let folder = tempfile::tempdir().expect("a temporary folder");
    let fasta = folder.path().join("genome.fa");
    std::fs::write(&fasta, ">a\n").expect("writes the file");
    let url = cwl_parser::iri::file_url(&fasta);
    let job = |url: &str| match Value::parse(&format!(
        "{{reference: {{class: File, location: '{url}'}}}}"
    )) {
        Ok(Value::Map(map)) => map,
        other => panic!("{other:?}"),
    };
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let setting = setting(&types, &evaluator, &assumed);
    let missing = prepare(&tool, job(&url), &setting);
    assert!(missing.is_err_and(|error| error.contains("genome.fa.fai")));
    std::fs::write(folder.path().join("genome.fa.fai"), "").expect("writes the index");
    assert!(prepare(&tool, job(&url), &setting).is_ok());
    // A file that is not on disk, as in a simulated run, counts as present with its neighbours.
    assert!(prepare(&tool, job("file:///nowhere/genome.fa"), &setting).is_ok());
}

#[test]
fn assumed_adds_secondaries() {
    let tool = process(INDEXED);
    let job = |url: &str| match Value::parse(&format!(
        "{{reference: {{class: File, location: '{url}'}}}}"
    )) {
        Ok(Value::Map(map)) => map,
        other => panic!("{other:?}"),
    };
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let inner = Setting {
        toplevel: false,
        discover: false,
        ..setting(&types, &evaluator, &assumed)
    };
    let given = prepare(&tool, job("file:///in/genome.fa"), &inner);
    let given = given.unwrap_or_else(|error| panic!("{error}"));
    let secondary = given
        .get("reference")
        .and_then(|file| file.get("secondaryFiles"));
    assert_eq!(
        secondary.and_then(Value::as_list).map(<[Value]>::len),
        Some(1)
    );
    let other = prepare(&tool, job("file:///work/3/genome.fa"), &inner);
    assert!(other.is_err_and(|error| error.contains("genome.fa.fai")));
}

#[test]
fn selects_enum_symbols() {
    let tool = process(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: run\noutputs: []\n\
         inputs:\n  mode: {type: {type: enum, symbols: [growth, dosage]}}\n",
    );
    let (types, evaluator) = (Types::default(), Evaluator::default());
    let assumed = [BASE.to_owned()];
    let strings = HashSet::from(["freud_sim".to_owned()]);
    let inner = Setting {
        toplevel: false,
        placeholder_strings: &strings,
        ..setting(&types, &evaluator, &assumed)
    };
    let job = |mode: &str| match Value::parse(&format!("{{mode: {mode}}}")) {
        Ok(Value::Map(map)) => map,
        other => panic!("{other:?}"),
    };
    let given = prepare(&tool, job("freud_sim"), &inner).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(given.get("mode"), Some(&Value::string("growth")));
    // Other strings are real values, which must fit.
    assert!(prepare(&tool, job("other"), &inner).is_err());
}
