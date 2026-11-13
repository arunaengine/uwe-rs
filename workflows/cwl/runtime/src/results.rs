//! Delivers real CWL output files and directories and rewrites their result locations.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use cwl_parser::iri;
use workflow_graph::{Delivery, DeliveryMode};

use crate::files;
use crate::value::{Map, Value};

/// Plans output delivery and rewrites result locations to their destinations.
/// Literals are written locally; remote callers materialize them before delivery.
pub fn deliver(outputs: &mut Map, folder: &Path) -> Result<Vec<Delivery>, String> {
    let mut deliveries = Vec::new();
    let mut used = HashSet::new();
    for (_, value) in outputs.iter_mut() {
        relocate(value, folder, &mut deliveries, &mut used, false)?;
    }
    Ok(deliveries)
}

/// Materializes literals before remote delivery; referenced children are copied first.
pub fn materialize(outputs: &mut Map, folder: &Path) -> Result<Vec<Delivery>, String> {
    fn visit(
        value: &mut Value,
        folder: &Path,
        deliveries: &mut Vec<Delivery>,
        used: &mut HashSet<PathBuf>,
    ) -> Result<(), String> {
        if value.is_file_like()
            && value
                .text("location")
                .is_none_or(|location| location.starts_with("_:"))
        {
            return relocate(value, folder, deliveries, used, false);
        }
        match value {
            Value::Map(map) => {
                for value in map.values_mut() {
                    visit(value, folder, deliveries, used)?;
                }
            }
            Value::List(items) => {
                for value in items {
                    visit(value, folder, deliveries, used)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut deliveries = Vec::new();
    let mut used = HashSet::new();
    for value in outputs.values_mut() {
        visit(value, folder, &mut deliveries, &mut used)?;
    }
    Ok(deliveries)
}

fn relocate(
    value: &mut Value,
    folder: &Path,
    deliveries: &mut Vec<Delivery>,
    used: &mut HashSet<PathBuf>,
    grouped: bool,
) -> Result<(), String> {
    match value {
        Value::List(items) => {
            for item in items {
                relocate(item, folder, deliveries, used, grouped)?;
            }
        }
        Value::Map(map) if is_filesystem_value(map) => {
            let mut names = vec![basename(map)?];
            if let Some(Value::List(secondary)) = map.get("secondaryFiles") {
                for item in secondary {
                    if let Value::Map(file) = item {
                        names.push(basename(file)?);
                    }
                }
            }
            if names.iter().collect::<HashSet<_>>().len() != names.len() {
                return Err("an output and its secondary files have duplicate basenames".into());
            }
            let mut selected = folder.to_owned();
            if !grouped && names.iter().any(|name| used.contains(&folder.join(name))) {
                let mut number = 2;
                loop {
                    selected = folder.join(format!("{}-{number}", names[0]));
                    if !used.contains(&selected) {
                        break;
                    }
                    number += 1;
                }
                used.insert(selected.clone());
            }
            let folder = selected.as_path();
            for name in &names {
                let target = folder.join(name);
                destination(&target)?;
                used.insert(target);
            }
            let location = map.get("location").and_then(Value::as_str);
            if location.is_none_or(|location| location.starts_with("_:")) {
                write_literal(map, folder, deliveries, used)?;
                return Ok(());
            }
            let source = location
                .and_then(files::local_path)
                .unwrap_or_else(|| PathBuf::from(location.unwrap_or_default()));
            let basename = match map.get("basename").and_then(Value::as_str) {
                Some(name) => name.to_owned(),
                None => files::basename_of(location.unwrap_or_default()),
            };
            let target = folder.join(&basename);
            deliveries.push(Delivery {
                overwrite: true,
                location: source.to_string_lossy().into_owned(),
                target: target.to_string_lossy().into_owned(),
                mode: DeliveryMode::Copy,
            });
            name(map, &source, &target);
            if let Some(Value::List(secondary)) = map.get_mut("secondaryFiles") {
                for item in secondary {
                    relocate(item, folder, deliveries, used, true)?;
                }
            }
        }
        // A record holds its fields.
        Value::Map(map) => {
            for (_, item) in map.iter_mut() {
                relocate(item, folder, deliveries, used, grouped)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn basename(map: &Map) -> Result<String, String> {
    let name = map
        .get("basename")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            files::basename_of(map.get("location").and_then(Value::as_str).unwrap_or(""))
        });
    let mut parts = Path::new(&name).components();
    if name.contains('/')
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
    {
        return Err(format!(
            "the output basename `{name}` must be one file name"
        ));
    }
    Ok(name)
}

fn destination(path: &Path) -> Result<(), String> {
    if path.to_string_lossy().contains("://") {
        return Ok(());
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "the output destination {} is a symbolic link",
                    ancestor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect {}: {error}", ancestor.display())),
        }
    }
    Ok(())
}

/// Writes a file literal with its `contents`, or makes a directory literal with its `listing`
/// inside it, under its basename in a local folder, and names it there.
fn write_literal(
    map: &mut Map,
    folder: &Path,
    deliveries: &mut Vec<Delivery>,
    used: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    if folder.to_string_lossy().contains("://") {
        return Err("literal CWL outputs need a local output folder".into());
    }
    let target = folder.join(basename(map)?);
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
    written.map_err(|error| format!("cannot write {}: {error}", target.display()))?;
    map.insert("location".into(), Value::String(iri::file_url(&target)));
    map.insert(
        "path".into(),
        Value::String(target.to_string_lossy().into_owned()),
    );
    if let Some(Value::List(listing)) = map.get_mut("listing") {
        for item in listing {
            relocate(item, &target, deliveries, used, false)?;
        }
    }
    if let Some(Value::List(secondary)) = map.get_mut("secondaryFiles") {
        for item in secondary {
            relocate(item, folder, deliveries, used, true)?;
        }
    }
    Ok(())
}

fn is_filesystem_value(map: &Map) -> bool {
    matches!(
        map.get("class").and_then(Value::as_str),
        Some("File" | "Directory")
    )
}

/// Names a file or directory at its new place, with the entries of its `listing` inside it.
fn name(map: &mut Map, source: &Path, target: &Path) {
    let location = if target.to_string_lossy().contains("://") {
        target.to_string_lossy().into_owned()
    } else {
        iri::file_url(target)
    };
    map.insert("location".into(), Value::String(location));
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
        let Some(inner) = inner
            .map(|location| files::local_path(location).unwrap_or_else(|| PathBuf::from(location)))
        else {
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
    fn delivers_output_basenames() {
        let text = "{out: {class: File, location: 'file:///s/runs/a/work/x.txt', basename: \
                    x.txt, secondaryFiles: [{class: File, location: 'file:///s/runs/a/work/x.txt.idx'}]}, \
                    dir: {class: Directory, location: 'file:///s/runs/b/work/d', listing: \
                    [{class: File, location: 'file:///s/runs/b/work/d/e.txt'}]}, n: 3}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("a map")
        };
        let deliveries = deliver(&mut outputs, Path::new("/out")).expect("deliveries");
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
    fn writes_output_literals() {
        // The conformance tests exprtool_file_literal and exprtool_directory_literal.
        let folder = tempfile::tempdir().expect("a temporary directory");
        let text = "{lit: {class: File, basename: a_file, contents: 'Hello', location: '_:1'}, \
                    dir: {class: Directory, basename: a_directory, location: '_:2', listing: \
                    [{class: File, location: 'file:///s/runs/a/work/hello.txt', basename: \
                    hello.txt}]}}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("a map")
        };
        let deliveries = deliver(&mut outputs, folder.path()).expect("deliveries");
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

    #[test]
    fn rejects_delivery_escapes() {
        let root = tempfile::tempdir().expect("temporary directory");
        let out = root.path().join("out");
        std::fs::create_dir(&out).expect("output folder");
        let sentinel = root.path().join("sentinel");
        std::fs::write(&sentinel, "original").expect("sentinel");
        for basename in [
            "../sentinel".to_owned(),
            sentinel.to_string_lossy().into_owned(),
        ] {
            let mut file = Map::new();
            file.insert("class".into(), Value::string("File"));
            file.insert("basename".into(), Value::String(basename));
            file.insert("contents".into(), Value::string("replacement"));
            let mut outputs = Map::new();
            outputs.insert("out".into(), Value::Map(file));
            assert!(deliver(&mut outputs, &out).is_err());
        }
        std::os::unix::fs::symlink(&sentinel, out.join("linked")).expect("symlink");
        for text in [
            "{out: {class: File, basename: linked, contents: replacement}}",
            "{out: {class: Directory, basename: dir, listing: [{class: File, basename: ../../sentinel, contents: replacement}]}}",
        ] {
            let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
                panic!("map")
            };
            assert!(deliver(&mut outputs, &out).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(sentinel).expect("sentinel"),
            "original"
        );
    }

    #[test]
    fn groups_colliding_secondaries() {
        let root = tempfile::tempdir().expect("temporary directory");
        let text = "{out: [{class: File, basename: result.txt, contents: first, secondaryFiles: [{class: File, basename: result.txt.idx, contents: first-index}]}, {class: File, basename: result.txt, contents: second, secondaryFiles: [{class: File, basename: result.txt.idx, contents: second-index}]}]}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("map")
        };
        deliver(&mut outputs, root.path()).expect("deliveries");
        let Value::List(items) = &outputs["out"] else {
            panic!("list")
        };
        let first = files::local_path(items[0].text("location").expect("location")).expect("path");
        let second = files::local_path(items[1].text("location").expect("location")).expect("path");
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).expect("first"), "first");
        assert_eq!(std::fs::read_to_string(&second).expect("second"), "second");
        assert_eq!(
            std::fs::read_to_string(second.with_file_name("result.txt.idx")).expect("index"),
            "second-index"
        );
        let text = "{out: [{class: File, basename: result.txt, location: 'file:///first/result.txt'}, {class: File, basename: result.txt, location: 'file:///second/result.txt'}]}";
        let Value::Map(mut outputs) = Value::parse(text).expect("YAML") else {
            panic!("map")
        };
        let deliveries = deliver(&mut outputs, root.path()).expect("deliveries");
        assert_ne!(deliveries[0].target, deliveries[1].target);
    }

    #[test]
    fn preserves_remote_destinations() {
        let Value::Map(mut outputs) = Value::parse("{out: {class: Directory, location: 'file:///source/tree', listing: [{class: File, location: 'file:///source/tree/child'}]}} ").expect("outputs") else { panic!("map") };
        let deliveries = deliver(&mut outputs, Path::new("s3://bucket/results")).expect("delivery");
        assert_eq!(deliveries[0].target, "s3://bucket/results/tree");
        assert_eq!(
            outputs["out"].text("location"),
            Some(deliveries[0].target.as_str())
        );
        let Value::List(children) = outputs["out"].get("listing").expect("listing") else {
            panic!("list")
        };
        assert_eq!(
            children[0].text("location"),
            Some("s3://bucket/results/tree/child")
        );
    }

    #[test]
    fn stages_remote_literals() {
        let root = tempfile::tempdir().expect("folder");
        let source = root.path().join("source");
        std::fs::write(&source, "referenced bytes").expect("source");
        let text =
            serde_json::json!({"out": {"class": "Directory", "basename": "tree", "listing": [
                {"class": "File", "basename": "literal", "contents": "literal bytes"},
                {"class": "File", "basename": "reference", "location": iri::file_url(&source)}
            ]}})
            .to_string();
        let Value::Map(mut outputs) = Value::parse(&text).expect("outputs") else {
            panic!("map")
        };
        let local = root.path().join("materialized");
        let mut deliveries = materialize(&mut outputs, &local).expect("materialized");
        assert_eq!(deliveries.len(), 1);
        let staged = deliveries.remove(0);
        std::fs::copy(staged.location, staged.target).expect("referenced child delivered first");
        let deliveries =
            deliver(&mut outputs, Path::new("s3://bucket/results")).expect("remote delivery");
        assert_eq!(deliveries.len(), 1);
        let source = Path::new(&deliveries[0].location);
        assert_eq!(
            std::fs::read_to_string(source.join("literal")).expect("literal"),
            "literal bytes"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("reference")).expect("reference"),
            "referenced bytes"
        );
        assert_eq!(
            outputs["out"].text("location"),
            Some("s3://bucket/results/tree")
        );
    }

    #[test]
    fn propagates_literal_errors() {
        let root = tempfile::tempdir().expect("temporary directory");
        let out = root.path().join("file");
        std::fs::write(&out, "existing").expect("file");
        let Value::Map(mut outputs) =
            Value::parse("{out: {class: File, basename: result, contents: text}}").expect("YAML")
        else {
            panic!("map")
        };
        assert!(deliver(&mut outputs, &out).is_err());
        assert_eq!(std::fs::read_to_string(out).expect("file"), "existing");
    }
}
