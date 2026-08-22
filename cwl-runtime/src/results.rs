//! The results of a real run, as `cwltool --outdir` gives them: each file and directory of the
//! output object goes to the output folder under its basename, and the output object then
//! names it there. The files of a directory come with it, and secondary files go next to
//! their primary file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::iri;
use workflow_graph::{Delivery, DeliveryMode};

use crate::files;
use crate::value::{Map, Value};

/// The deliveries of the files of an output object to `folder`; the object then names the
/// files at their new places. A literal, a file or directory that has no place yet, is
/// written to a local folder at once, as cwltool does; a remote file stays as it is.
pub fn deliver(outputs: &mut Map, folder: &Path) -> Vec<Delivery> {
    let mut deliveries = Vec::new();
    for (_, value) in outputs.iter_mut() {
        relocate(value, folder, &mut deliveries);
    }
    deliveries
}

fn relocate(value: &mut Value, folder: &Path, deliveries: &mut Vec<Delivery>) {
    match value {
        Value::List(items) => {
            for item in items {
                relocate(item, folder, deliveries);
            }
        }
        Value::Map(map) if is_file_or_directory(map) => {
            let location = map.get("location").and_then(Value::as_str);
            if location.is_none_or(|location| location.starts_with("_:")) {
                write_literal(map, folder, deliveries);
                return;
            }
            let Some(source) = location.and_then(files::local_path) else {
                return;
            };
            let basename = match map.get("basename").and_then(Value::as_str) {
                Some(name) => name.to_owned(),
                None => files::basename_of(location.unwrap_or_default()),
            };
            let target = folder.join(&basename);
            deliveries.push(Delivery {
                location: source.to_string_lossy().into_owned(),
                target: target.to_string_lossy().into_owned(),
                mode: DeliveryMode::Copy,
            });
            name(map, &source, &target);
            if let Some(Value::List(secondary)) = map.get_mut("secondaryFiles") {
                for item in secondary {
                    relocate(item, folder, deliveries);
                }
            }
        }
        // A record holds its fields.
        Value::Map(map) => {
            for (_, item) in map.iter_mut() {
                relocate(item, folder, deliveries);
            }
        }
        _ => {}
    }
}

/// Writes a file literal with its `contents`, or makes a directory literal with its `listing`
/// inside it, under its basename in a local folder, and names it there.
fn write_literal(map: &mut Map, folder: &Path, deliveries: &mut Vec<Delivery>) {
    let basename = map.get("basename").and_then(Value::as_str);
    let Some(basename) = basename.filter(|_| !folder.to_string_lossy().contains("://")) else {
        return;
    };
    let target = folder.join(basename);
    let directory = map.get("class").and_then(Value::as_str) == Some("Directory");
    let written = match directory {
        true => std::fs::create_dir_all(&target),
        false => {
            let contents = map
                .get("contents")
                .and_then(Value::as_str)
                .unwrap_or_default();
            std::fs::create_dir_all(folder).and_then(|()| std::fs::write(&target, contents))
        }
    };
    if written.is_err() {
        return;
    }
    map.insert("location".into(), Value::String(iri::file_url(&target)));
    map.insert(
        "path".into(),
        Value::String(target.to_string_lossy().into_owned()),
    );
    if let Some(Value::List(listing)) = map.get_mut("listing") {
        for item in listing {
            relocate(item, &target, deliveries);
        }
    }
}

fn is_file_or_directory(map: &Map) -> bool {
    matches!(
        map.get("class").and_then(Value::as_str),
        Some("File" | "Directory")
    )
}

/// Names a file or directory at its new place, with the entries of its `listing` inside it.
fn name(map: &mut Map, source: &Path, target: &Path) {
    map.insert("location".into(), Value::String(iri::file_url(target)));
    map.insert(
        "path".into(),
        Value::String(target.to_string_lossy().into_owned()),
    );
    let Some(Value::List(listing)) = map.get_mut("listing") else {
        return;
    };
    for entry in listing {
        let Value::Map(entry) = entry else {
            continue;
        };
        let inner = entry.get("location").and_then(Value::as_str);
        let Some(inner) = inner.and_then(files::local_path) else {
            continue;
        };
        if let Ok(relative) = inner.strip_prefix(source) {
            let moved = target.join(relative);
            name(entry, &inner, &moved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_go_to_the_folder_under_their_basenames() {
        let text = "{out: {class: File, location: 'file:///s/runs/a/work/x.txt', basename: \
                    x.txt, secondaryFiles: [{class: File, location: 'file:///s/runs/a/work/x.txt.idx'}]}, \
                    dir: {class: Directory, location: 'file:///s/runs/b/work/d', listing: \
                    [{class: File, location: 'file:///s/runs/b/work/d/e.txt'}]}, n: 3}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("a map")
        };
        let deliveries = deliver(&mut outputs, Path::new("/out"));
        let moves: Vec<(&str, String)> = deliveries
            .iter()
            .map(|delivery| {
                let target = delivery.target.clone();
                (delivery.location.as_str(), target)
            })
            .collect();
        let expected = [
            ("/s/runs/a/work/x.txt", "/out/x.txt".to_owned()),
            ("/s/runs/a/work/x.txt.idx", "/out/x.txt.idx".to_owned()),
            ("/s/runs/b/work/d", "/out/d".to_owned()),
        ];
        assert_eq!(moves, expected);
        let text = Value::Map(outputs).to_json();
        assert!(text.contains("\"path\":\"/out/d/e.txt\""), "{text}");
        assert!(
            text.contains("\"location\":\"file:///out/x.txt\""),
            "{text}"
        );
    }

    #[test]
    fn literals_are_written_to_the_folder() {
        // The conformance tests exprtool_file_literal and exprtool_directory_literal.
        let folder = tempfile::tempdir().expect("a temporary directory");
        let text = "{lit: {class: File, basename: a_file, contents: 'Hello', location: '_:1'}, \
                    dir: {class: Directory, basename: a_directory, location: '_:2', listing: \
                    [{class: File, location: 'file:///s/runs/a/work/hello.txt', basename: \
                    hello.txt}]}}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("a map")
        };
        let deliveries = deliver(&mut outputs, folder.path());
        let file = folder.path().join("a_file");
        assert_eq!(std::fs::read_to_string(&file).expect("written"), "Hello");
        let directory = folder.path().join("a_directory");
        assert!(directory.is_dir());
        assert_eq!(deliveries.len(), 1);
        let target = directory.join("hello.txt").to_string_lossy().into_owned();
        assert_eq!(deliveries[0].target, target);
        let text = Value::Map(outputs).to_json();
        assert!(text.contains(&iri::file_url(&file)), "{text}");
        assert!(text.contains(&iri::file_url(&directory)), "{text}");
    }
}
