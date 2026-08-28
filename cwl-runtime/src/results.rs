//! The results of a real run, as `cwltool --outdir` gives them: each file and directory of the
//! output object goes to the output folder under its basename, and the output object then
//! names it there. The files of a directory come with it, and secondary files go next to
//! their primary file.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use cwl_parser::iri;
use workflow_graph::{Delivery, DeliveryMode};

use crate::files;
use crate::value::{Map, Value};

/// The deliveries of the files of an output object to `folder`; the object then names the
/// files at their new places. A literal, a file or directory that has no place yet, is
/// written to a local folder at once, as cwltool does; a remote file stays as it is.
pub fn deliver(outputs: &mut Map, folder: &Path) -> Result<Vec<Delivery>, String> {
    let mut deliveries = Vec::new();
    let mut used = HashSet::new();
    for (_, value) in outputs.iter_mut() {
        relocate(value, folder, &mut deliveries, &mut used, false)?;
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
        Value::Map(map) if is_file_or_directory(map) => {
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
            let Some(source) = location.and_then(files::local_path) else {
                return Ok(());
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
    fn escaping_basenames_and_symlink_destinations_leave_sentinels_unchanged() {
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
    fn colliding_outputs_keep_their_secondary_files_together() {
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
    fn literal_write_errors_fail_delivery() {
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
