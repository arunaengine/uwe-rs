//! Tests of the names of the jobs of a run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn names_scatter_elements_by_index() {
    let names = Names::default()
        .child("align")
        .child("sort")
        .indexed(&[1, 2]);
    assert_eq!(names.display, "align/sort[1,2]");
    assert_eq!(names.step, "align/sort");
}

#[test]
fn directory_aliases_refresh_separately_held_descendants() {
    let root = tempfile::tempdir().expect("folder");
    let original = root.path().join("original");
    let alias = root.path().join("alias");
    std::fs::create_dir_all(original.join("nested")).expect("directory");
    std::fs::write(original.join("nested/x #.txt"), "seed").expect("child");
    std::os::unix::fs::symlink(&original, &alias).expect("alias");
    let child = iri::file_url(&alias.join("nested/x #.txt"));
    let Value::Map(outputs) = Value::parse(
        &serde_json::json!({
            "directory": {"class": "Directory", "location": iri::file_url(&alias), "listing": []},
            "child": {"class": "File", "location": child, "contents": "seed", "size": 4}
        })
        .to_string(),
    )
    .expect("values") else {
        panic!("a map")
    };
    let mut run = Run::new(Options::default(), false);
    run.events.push_back(Event::Finished(0, outputs));
    run.update_in_place(&iri::file_url(&original), "s3://mirror/new");
    let Some(Event::Finished(_, outputs)) = run.events.pop_front() else {
        panic!("outputs")
    };
    assert_eq!(
        outputs["directory"].text("location"),
        Some("s3://mirror/new")
    );
    assert!(outputs["directory"].get("listing").is_none());
    assert_eq!(
        outputs["child"].text("location"),
        Some("s3://mirror/new/nested/x%20%23.txt")
    );
    assert!(outputs["child"].get("contents").is_none());
    assert!(outputs["child"].get("size").is_none());
}
