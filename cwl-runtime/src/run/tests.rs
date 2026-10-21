//! Tests of the names of the jobs of a run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn names_scatter_indexes() {
    let names = Names::default()
        .child("align")
        .child("sort")
        .indexed(&[1, 2]);
    assert_eq!(names.display, "align/sort[1,2]");
    assert_eq!(names.step, "align/sort");
}

#[test]
fn refreshes_directory_aliases() {
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

#[test]
fn detects_parent_conflicts() {
    let root = tempfile::tempdir().expect("folder");
    let parent = root.path().join("parent");
    let other = root.path().join("other");
    let unrelated = root.path().join("unrelated");
    std::fs::create_dir(&parent).expect("parent");
    std::fs::create_dir(&other).expect("other");
    std::fs::create_dir(&unrelated).expect("unrelated");
    std::fs::write(unrelated.join("independent.txt"), "other").expect("independent");
    let child = parent.join("child.txt");
    let alias = other.join("alias.txt");
    std::fs::write(&child, "seed").expect("child");
    std::fs::hard_link(&child, &alias).expect("hard link");
    let directory_alias = root.path().join("directory-alias");
    std::os::unix::fs::symlink(&parent, &directory_alias).expect("directory alias");
    let writer = iri::file_url(&child);
    let conflict = |location: &str, directory| {
        source_conflicts(
            location,
            directory,
            &writer,
            &mut 10_000,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("resolves")
    };
    assert!(conflict(&iri::file_url(&directory_alias), true));
    assert!(conflict(&iri::file_url(&alias), false));
    assert!(conflict(&iri::file_url(&other), true));
    assert!(!conflict(&iri::file_url(&unrelated), true));
    let error = source_conflicts(
        &iri::file_url(&unrelated),
        true,
        &writer,
        &mut 0,
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("unresolved alias scan must fail explicitly");
    assert!(
        error.contains("bounded directory scan exhausted"),
        "{error}"
    );
    let view = Value::parse(
        &serde_json::json!({
            "class": "Directory", "location": iri::file_url(&other),
            "listing": [{"class": "File", "location": iri::file_url(&alias)}]
        })
        .to_string(),
    )
    .expect("view");
    let mut run = Run::new(Options::default(), false);
    run.in_place_sources.insert(writer);
    assert!(run.observe_directories(&view).is_err());
}
