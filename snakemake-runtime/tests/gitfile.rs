//! Reads Snakefiles of `gitfile(...)` from git repositories of this machine with gix and from
//! repositories on the web with the Git client of the run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::{Arc, Mutex};

use gix::objs::tree::{Entry, EntryKind};
use workflow_graph::Launch;

/// A commit of `rules/main.smk`, which includes `other.smk` beside it, whose rule echoes
/// `word`.
fn commit(repository: &gix::Repository, word: &str, parents: Vec<gix::ObjectId>) -> gix::ObjectId {
    let blob = |text: &str| repository.write_blob(text).expect("a blob").detach();
    let entry = |name: &str, kind: EntryKind, oid| Entry {
        mode: kind.into(),
        filename: name.into(),
        oid,
    };
    let rule = format!("rule a:\n    output: 'a.txt'\n    shell: 'echo {word} > {{output}}'\n");
    let rules = gix::objs::Tree {
        entries: vec![
            entry("main.smk", EntryKind::Blob, blob("include: 'other.smk'\n")),
            entry("other.smk", EntryKind::Blob, blob(&rule)),
        ],
    };
    let rules = repository.write_object(&rules).expect("a tree").detach();
    let top = gix::objs::Tree {
        entries: vec![entry("rules", EntryKind::Tree, rules)],
    };
    let top = repository.write_object(&top).expect("a tree").detach();
    let person = gix::actor::SignatureRef {
        name: "test".into(),
        email: "test@example.org".into(),
        time: "0 +0000",
    };
    let commit = repository.commit_as(person, person, "refs/heads/main", word, top, parents);
    commit.expect("a commit").detach()
}

fn script(folder: &Path, snakefile: &str, settings: snakemake_runtime::Settings) -> String {
    let snakefile = format!("rule all:\n    input: 'a.txt'\n{snakefile}");
    std::fs::write(folder.join("Snakefile"), snakefile).expect("a Snakefile");
    let launch = Launch {
        target: folder.to_path_buf(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver_with(&launch, settings).expect("a driver");
    match driver.start() {
        Ok(tasks) => format!("{tasks:?}"),
        Err(error) => error.message,
    }
}

/// Snakemake reads `rules/main.smk` of the repository at tag `v1`, before the branch `main`
/// moved on, and the file it includes from the same commit.
#[test]
fn reads_local_repositories() {
    let folder = tempfile::tempdir().expect("a folder");
    let repository = gix::init(folder.path().join("repo")).expect("a repository");
    let first = commit(&repository, "one", Vec::new());
    let tag = gix::refs::transaction::PreviousValue::Any;
    repository.tag_reference("v1", first, tag).expect("a tag");
    commit(&repository, "two", vec![first]);
    let read = |reference: &str| {
        let snakefile = format!("include: gitfile('repo', 'rules/main.smk', {reference})\n");
        script(folder.path(), &snakefile, Default::default())
    };
    assert!(
        read("tag='v1'").contains("echo one > a.txt"),
        "{}",
        read("tag='v1'")
    );
    assert!(read("branch='main'").contains("echo two > a.txt"));
    assert!(read(&format!("commit='{first}'")).contains("echo one > a.txt"));
    let missing = read("tag='v9'");
    assert!(
        missing.contains("Failed to read source") && missing.contains("v9"),
        "{missing}"
    );
}

/// A repository on the web goes to the Git client of the run once per run, with its address
/// and reference, and its files are read like those of a repository here.
#[test]
fn fetches_web_repositories() {
    let folder = tempfile::tempdir().expect("a folder");
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let checkout = move |url: &str, reference: &str, target: &Path| {
        seen.lock().unwrap().push(format!("{url}@{reference}"));
        std::fs::create_dir_all(target.join("rules")).map_err(|error| error.to_string())?;
        let rule = "rule a:\n    output: 'a.txt'\n    shell: 'echo web > {output}'\n";
        std::fs::write(target.join("rules/main.smk"), "include: 'other.smk'\n").unwrap();
        std::fs::write(target.join("rules/other.smk"), rule).unwrap();
        Ok(())
    };
    let settings = snakemake_runtime::Settings {
        git: Some(snakemake_runtime::GitClient(Arc::new(checkout))),
        ..Default::default()
    };
    let snakefile =
        "include: gitfile('https://example.org/repo.git', 'rules/main.smk', branch='dev')\n";
    let text = script(folder.path(), snakefile, settings);
    assert!(text.contains("echo web > a.txt"), "{text}");
    assert_eq!(*asked.lock().unwrap(), ["https://example.org/repo.git@dev"]);
    let refused = script(folder.path(), snakefile, Default::default());
    assert!(refused.contains("no Git client"), "{refused}");
}

/// A crafted tree that holds a link `a` to a folder outside and a folder `a` with a file
/// writes nothing outside: it is refused. Links that lead out become plain files.
#[test]
fn contains_crafted_trees() {
    let folder = tempfile::tempdir().expect("a folder");
    let outside = folder.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let repository = gix::init(folder.path().join("repo")).expect("a repository");
    let blob = |text: &str| repository.write_blob(text).expect("a blob").detach();
    let entry = |name: &str, kind: EntryKind, oid| Entry {
        mode: kind.into(),
        filename: name.into(),
        oid,
    };
    let write = |entries| {
        repository
            .write_object(&gix::objs::Tree { entries })
            .unwrap()
    };
    let rule = "rule a:\n    output: 'a.txt'\n    shell: 'echo safe > {output}'\n";
    let rules = write(vec![entry("main.smk", EntryKind::Blob, blob(rule))]).detach();
    let inner = write(vec![entry(".bashrc", EntryKind::Blob, blob("evil"))]).detach();
    let target = outside.to_string_lossy().into_owned();
    let person = gix::actor::SignatureRef {
        name: "test".into(),
        email: "test@example.org".into(),
        time: "0 +0000",
    };
    let crafted = write(vec![
        entry("a", EntryKind::Link, blob(&target)),
        entry("a", EntryKind::Tree, inner),
        entry("rules", EntryKind::Tree, rules),
    ]);
    let commit = repository.commit_as(
        person,
        person,
        "refs/heads/bad",
        "bad",
        crafted,
        Vec::<gix::ObjectId>::new(),
    );
    commit.expect("a commit");
    let links = write(vec![
        entry("leak", EntryKind::Link, blob(&target)),
        entry("near", EntryKind::Link, blob("rules/main.smk")),
        entry("rules", EntryKind::Tree, rules),
    ]);
    let commit = repository.commit_as(
        person,
        person,
        "refs/heads/good",
        "good",
        links,
        Vec::<gix::ObjectId>::new(),
    );
    commit.expect("a commit");
    let read = |branch: &str| {
        let snakefile = format!("include: gitfile('repo', 'rules/main.smk', tag='{branch}')\n");
        script(folder.path(), &snakefile, Default::default())
    };
    let refused = read("bad");
    assert!(refused.contains("twice"), "{refused}");
    assert!(
        !outside.join(".bashrc").exists(),
        "nothing is written outside"
    );
    let text = read("good");
    assert!(text.contains("echo safe > a.txt"), "{text}");
    let checkout = std::fs::read_dir(folder.path().join("scratch/downloads/snakemake/git"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("leak").exists())
        .expect("the checkout");
    assert!(
        !checkout.join("leak").is_symlink(),
        "an outward link is a file"
    );
    assert!(checkout.join("near").is_symlink(), "an inward link stays");
}
