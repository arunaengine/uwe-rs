//! Checks pinned presets, input overrides and installed reference verification.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::json;
use task_model::{Digest, Tree, TreeEntry};
use uwe::catalogue::Catalogue;
use uwe::cli::Arguments;

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("project")).unwrap();
        readonly(&root.path().join("project/main.nf"), b"workflow {}\n");
        std::fs::set_permissions(
            root.path().join("project"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        readonly(&root.path().join("db.fa"), b"ACGT\n");
        std::fs::write(
            root.path().join("defaults.json"),
            r#"{"threads":2,"fixed":true}"#,
        )
        .unwrap();
        Self(root)
    }

    fn config(&self) -> String {
        format!(
            "[workflows.demo]\nsource = 'project/main.nf'\ndigest = '{}'\nlanguage = 'nextflow'\nprofiles = ['lab']\nexecutor = 'simulator'\nparams = 'defaults.json'\noverrides = ['threads']\n[workflows.demo.references]\ndatabase = 'release'\n[references.release]\npath = 'db.fa'\ndigest = '{}'\nversion = '1'\nfiles = []\n",
            tree_digest("main.nf", b"workflow {}\n"),
            Digest::sha256(b"ACGT\n")
        )
    }

    fn parse(&self, text: &str) -> Result<Catalogue, uwe::Failure> {
        Catalogue::parse(text, &self.0.path().join("catalogue.toml"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(
            self.0.path().join("project"),
            std::fs::Permissions::from_mode(0o755),
        );
    }
}

fn readonly(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
}

fn tree_digest(name: &str, bytes: &[u8]) -> Digest {
    Tree {
        entries: vec![TreeEntry::File {
            path: name.into(),
            digest: Digest::sha256(bytes),
            size: bytes.len() as u64,
            executable: false,
        }],
    }
    .digest()
    .unwrap()
}

fn arguments() -> Arguments {
    Arguments {
        preset: Some("demo".into()),
        ..Arguments::default()
    }
}

#[test]
fn prepares_installed_preset() {
    let fixture = Fixture::new();
    let catalogue = fixture.parse(&fixture.config()).unwrap();
    let posted = fixture.0.path().join("posted.json");
    std::fs::write(&posted, r#"{"threads":3}"#).unwrap();
    let mut arguments = arguments();
    arguments.inputs = Some(posted);
    arguments.params.push(("threads".into(), "4".into()));
    let prepared = catalogue.prepare(&mut arguments).unwrap().unwrap();
    let values: serde_json::Value = serde_json::from_reader(prepared.inputs.as_file()).unwrap();
    assert_eq!(
        values,
        json!({"threads":3,"fixed":true,"database":fixture.0.path().join("db.fa")})
    );
    assert_eq!(arguments.params, [("threads".into(), "4".into())]);
    assert_eq!(
        arguments.target,
        Some(fixture.0.path().join("project/main.nf"))
    );
    assert_eq!(arguments.profiles, Some(vec!["lab".into()]));
    assert_eq!(arguments.executor.as_deref(), Some("simulator"));
    assert!(arguments.preset.is_none());
    assert!(catalogue.prepare(&mut arguments).unwrap().is_none());
    assert_eq!(catalogue.list()["references"][0]["version"], "1");
    assert_eq!(catalogue.list()["version"], 1);
    let executors = &catalogue.list()["capabilities"]["executors"];
    assert_eq!(executors["tes"], json!([]));
    for name in ["slurm", "htcondor", "pbs", "nomad", "wr"] {
        assert_eq!(executors[name], json!(["offline"]), "{name}");
    }
    for name in [
        "aws-batch",
        "google-batch",
        "azure-batch",
        "flyte",
        "snowflake",
        "jarvice",
    ] {
        assert_eq!(executors[name], json!([]), "{name}");
    }
}

#[test]
fn checks_executor_requirements() {
    let fixture = Fixture::new();
    let text = fixture.config().replace(
        "executor = 'simulator'",
        "executor = 'local'\nexecutors = ['local', 'slurm']\nrequires = ['sidecars']",
    );
    let catalogue = fixture.parse(&text).unwrap();
    assert!(catalogue.prepare(&mut arguments()).is_ok());
    let mut args = arguments();
    args.executor = Some("tes".into());
    assert!(
        catalogue
            .prepare(&mut args)
            .err()
            .unwrap()
            .message
            .contains("does not allow executor")
    );
    args.executor = Some("slurm".into());
    assert!(
        catalogue
            .prepare(&mut args)
            .err()
            .unwrap()
            .message
            .contains("requires `sidecars`")
    );
    assert!(
        fixture
            .parse(&text.replace("'sidecars'", "'unknown'"))
            .is_err()
    );
    assert!(
        fixture
            .parse(&text.replace("executor = 'local'\n", ""))
            .is_err()
    );
}

#[test]
fn rejects_forbidden_overrides() {
    let fixture = Fixture::new();
    let catalogue = fixture.parse(&fixture.config()).unwrap();
    for key in ["database", "fixed", "unknown"] {
        let mut arguments = arguments();
        arguments.params.push((key.into(), "other".into()));
        assert!(
            catalogue
                .prepare(&mut arguments)
                .err()
                .unwrap()
                .message
                .contains("does not allow overriding")
        );
        let posted = fixture.0.path().join("posted.json");
        std::fs::write(&posted, json!({key: "other"}).to_string()).unwrap();
        arguments.params.clear();
        arguments.inputs = Some(posted);
        assert!(catalogue.prepare(&mut arguments).is_err());
    }
    let mut arguments = arguments();
    arguments.preset = Some("missing".into());
    assert!(
        catalogue
            .prepare(&mut arguments)
            .err()
            .unwrap()
            .message
            .contains("unknown workflow")
    );
    arguments.preset = Some("demo".into());
    arguments.target = Some("other.nf".into());
    assert!(catalogue.prepare(&mut arguments).is_err());
}

#[test]
fn validates_catalogue_configuration() {
    let fixture = Fixture::new();
    let config = fixture.config();
    for bad in [
        config.replace("language = 'nextflow'", "language = 'unknown'"),
        config.replace("profiles = ['lab']", "profiles = 2"),
        config.replace("executor = 'simulator'", "executor = 'unknown'"),
        config.replace("version = '1'", "version = ''"),
        config.replace("files = []", "files = ['../db.fa']"),
        config.replace("database = 'release'", "database = 'missing'"),
        config.replace("version = '1'", "version = '1'\nunknown = true"),
        config.replace(
            "source = 'project/main.nf'",
            "source = 'project/main.nf'\nunknown = true",
        ),
    ] {
        assert!(fixture.parse(&bad).is_err(), "{bad}");
    }
    assert!(
        fixture
            .parse("[workflows.demo]\nsource = 'team/pipeline'\nrevision = 'main'\n")
            .is_err()
    );
    let pinned = format!(
        "[workflows.demo]\nsource = 'team/pipeline'\nrevision = '{}'\n",
        "a".repeat(40)
    );
    assert!(fixture.parse(&pinned).is_ok());
}

#[test]
fn verifies_reference_installation() {
    let fixture = Fixture::new();
    let config = fixture.config();
    for bad in [
        config.replace(
            &Digest::sha256(b"ACGT\n").to_string(),
            &Digest::sha256(b"wrong").to_string(),
        ),
        config.replace("files = []", "files = ['missing.idx']"),
    ] {
        assert!(
            fixture
                .parse(&bad)
                .unwrap()
                .prepare(&mut arguments())
                .is_err()
        );
    }
    std::fs::set_permissions(
        fixture.0.path().join("db.fa"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(
        fixture
            .parse(&config)
            .unwrap()
            .prepare(&mut arguments())
            .is_err()
    );
}

#[test]
fn pins_project_dependencies() {
    let fixture = Fixture::new();
    let catalogue = fixture.parse(&fixture.config()).unwrap();
    let project = fixture.0.path().join("project");
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    readonly(&project.join("module.nf"), b"changed dependency\n");
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert!(catalogue.prepare(&mut arguments()).is_err());
}

#[test]
fn resolves_overridden_origins() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let first = fixture.0.path().join("catalogue.toml");
    let second = other.0.path().join("override.toml");
    std::fs::write(&first, fixture.config()).unwrap();
    std::fs::write(&second, "[workflows.demo]\nsource = 'project/main.nf'\nparams = 'defaults.json'\n[references.release]\npath = 'db.fa'\n").unwrap();
    std::fs::write(other.0.path().join("defaults.json"), r#"{"threads":8}"#).unwrap();
    let config = config_file::ConfigFile::read(&[first, second]).unwrap();
    let catalogue = Catalogue::from_config(config).unwrap();
    let mut arguments = arguments();
    let prepared = catalogue.prepare(&mut arguments).unwrap().unwrap();
    let values: serde_json::Value = serde_json::from_reader(prepared.inputs.as_file()).unwrap();
    assert_eq!(
        arguments.target,
        Some(other.0.path().join("project/main.nf"))
    );
    assert_eq!(values["threads"], 8);
    assert_eq!(values["database"], json!(other.0.path().join("db.fa")));
}

#[test]
fn confines_reference_files() {
    let fixture = Fixture::new();
    let config = fixture
        .config()
        .replace("path = 'db.fa'", "path = 'project'")
        .replace(
            &Digest::sha256(b"ACGT\n").to_string(),
            &tree_digest("main.nf", b"workflow {}\n").to_string(),
        )
        .replace("files = []", "files = ['main.nf']");
    assert!(
        fixture
            .parse(&config)
            .unwrap()
            .prepare(&mut arguments())
            .is_ok()
    );
    let project = fixture.0.path().join("project");
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("../db.fa", project.join("escape")).unwrap();
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
    let config = config.replace("files = ['main.nf']", "files = ['escape']");
    assert!(
        fixture
            .parse(&config)
            .unwrap()
            .prepare(&mut arguments())
            .is_err()
    );
}
