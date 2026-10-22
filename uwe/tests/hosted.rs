//! Checks hosted tenant paths, preset restrictions and injected authorization.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use uwe::Failure;
use uwe::application::{self, Description, Policy};
use uwe::hosted::Hosted;
use workflow_graph::{Inputs, Language};

fn policy(tenant: &str, base: &Path) -> Result<Hosted, Failure> {
    Hosted::new(
        tenant,
        base,
        BTreeSet::from(["approved".into()]),
        Arc::new(|_| Ok(())),
    )
}

#[test]
fn separates_tenant_paths() {
    let base = tempfile::tempdir().unwrap();
    let first = policy("tenant_a", base.path())
        .unwrap()
        .check(Some("approved"))
        .unwrap();
    let second = policy("tenant_b", base.path())
        .unwrap()
        .check(Some("approved"))
        .unwrap();
    let base = base.path().canonicalize().unwrap();
    assert_eq!(first, base.join("tenant_a"));
    assert_eq!(second, base.join("tenant_b"));
    for child in ["cache", "runs.sqlite", "outputs"] {
        assert_ne!(first.join(child), second.join(child));
        assert!(first.join(child).starts_with(&base));
        assert!(second.join(child).starts_with(&base));
    }
}

#[test]
fn rejects_unsafe_tenants() {
    let base = tempfile::tempdir().unwrap();
    for tenant in [
        "",
        ".",
        "..",
        "../other",
        "a/b",
        "a\\b",
        "/absolute",
        "with space",
    ] {
        assert!(policy(tenant, base.path()).is_err(), "{tenant}");
    }
}

#[test]
fn requires_approved_preset() {
    let base = tempfile::tempdir().unwrap();
    let hosted = policy("tenant", base.path()).unwrap();
    assert!(hosted.check(None).is_err());
    assert!(hosted.check(Some("")).is_err());
    assert!(hosted.check(Some("unapproved")).is_err());
    assert!(hosted.check(Some("approved")).is_ok());
    assert!(Hosted::new("tenant", base.path(), BTreeSet::new(), Arc::new(|_| Ok(()))).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_tenant_symlinks() {
    let base = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let hosted = policy("tenant", base.path()).unwrap();
    let root = base.path().join("tenant");
    std::os::unix::fs::symlink(outside.path(), &root).unwrap();
    assert!(hosted.check(Some("approved")).is_err());
    std::fs::remove_file(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    for child in ["cache", "runs.sqlite", "outputs"] {
        let path = root.join(child);
        std::os::unix::fs::symlink(outside.path().join("missing"), &path).unwrap();
        assert!(hosted.check(Some("approved")).is_err(), "{child}");
        std::fs::remove_file(path).unwrap();
    }
    let sibling = base.path().join("other_tenant");
    std::fs::create_dir(&sibling).unwrap();
    std::os::unix::fs::symlink(&sibling, root.join("cache")).unwrap();
    assert!(hosted.check(Some("approved")).is_err());
}

#[test]
fn invokes_host_authorization() {
    let base = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hosted = Hosted::new(
        "tenant",
        base.path(),
        BTreeSet::from(["approved".into()]),
        Arc::new(move |values| {
            observed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(values["count"], json!(2));
            Err(Failure::new("tenant cannot use this input"))
        }),
    )
    .unwrap();
    hosted.check(Some("approved")).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        hosted
            .authorize_values(&json!({"count":2}))
            .unwrap_err()
            .message,
        "tenant cannot use this input"
    );
    let admission = application::validate_json(
        Description {
            language: Language::Cwl,
            inputs: Some(Inputs {
                schema: json!({"type":"object", "properties":{"count":{"type":"integer"}}}),
                values: json!({"count":2}),
                ..Inputs::default()
            }),
            outputs: None,
        },
        false,
        Policy::Strict,
    )
    .unwrap();
    assert_eq!(
        hosted.authorize(&admission).unwrap_err().message,
        "tenant cannot use this input"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rejects_before_planning() {
    use std::os::unix::fs::PermissionsExt;
    use task_model::{Digest, Tree, TreeEntry};

    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let source = b"workflow { println file(params.sample).text }\n";
    std::fs::write(project.join("main.nf"), source).unwrap();
    std::fs::set_permissions(
        project.join("main.nf"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
    let digest = Tree {
        entries: vec![TreeEntry::File {
            path: "main.nf".into(),
            digest: Digest::sha256(source),
            size: source.len() as u64,
            executable: false,
        }],
    }
    .digest()
    .unwrap();
    let config = base.path().join("catalogue.toml");
    std::fs::write(base.path().join("defaults.json"), r#"{"fixed":true}"#).unwrap();
    std::fs::write(&config, format!("[workflows.approved]\nsource='project/main.nf'\ndigest='{digest}'\nlanguage='nextflow'\nexecutor='simulator'\nparams='defaults.json'\noverrides=['sample']\n")).unwrap();
    let submitted = json!({"sample":base.path().join("denied.txt")});
    let expected = submitted.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hosted = Hosted::new(
        "tenant",
        base.path(),
        ["approved".into()].into(),
        Arc::new(move |values| {
            observed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(values["sample"], expected["sample"]);
            assert_eq!(values["fixed"], true);
            Err(Failure::new("sample is outside tenant inputs"))
        }),
    )
    .unwrap();
    let mut settings = uwe::run::Settings::default();
    settings.preset = Some("approved".into());
    settings.hosted = Some(hosted);
    settings.inputs = Some(submitted);
    settings.config_file = Some(config);
    settings.setup = Some(uwe::setup::Setup::default());
    let result = uwe::RunRequest::new(settings).describe().await;
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        result.unwrap_err().message,
        "sample is outside tenant inputs"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!base.path().join("tenant").exists());
}
