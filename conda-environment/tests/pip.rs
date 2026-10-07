//! Resolves pip against a local fake Simple index, including conflicts and corrupted hashes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{self, Write};

use conda_environment::{Lock, Package, PipError, VirtualPackage, lock_pip, validate_pip};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

struct Index {
    root: tempfile::TempDir,
}

impl Index {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn add(&self, file: &str, metadata: &str) {
        let name = metadata
            .lines()
            .find_map(|line| line.strip_prefix("Name: "))
            .unwrap();
        let path = self.root.path().join(format!("simple/{name}/index.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut project: Value = std::fs::read(&path)
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or(json!({"files":[]}));
        let bytes = if file.ends_with(".whl") {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            archive
                .start_file(
                    format!("{name}-1.dist-info/METADATA"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            archive.write_all(metadata.as_bytes()).unwrap();
            archive.finish().unwrap().into_inner()
        } else {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("{name}/PKG-INFO"), metadata.as_bytes())
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap()
        };
        let folder = self.root.path().join("artifacts");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(file), &bytes).unwrap();
        std::fs::write(folder.join(format!("{file}.metadata")), metadata).unwrap();
        project["files"].as_array_mut().unwrap().push(json!({
            "filename":file, "url":format!("https://files.example/artifacts/{file}"),
            "hashes":{"sha256":hash(&bytes)}, "core-metadata":{"sha256":hash(metadata.as_bytes())}, "yanked":false
        }));
        std::fs::write(path, project.to_string()).unwrap();
    }

    fn fetch(&self, url: &str) -> io::Result<Vec<u8>> {
        let url = reqwest::Url::parse(url).unwrap();
        let path = self.root.path().join(url.path().trim_start_matches('/'));
        std::fs::read(if path.is_dir() {
            path.join("index.json")
        } else {
            path
        })
    }

    fn resolve(&self, requirements: &[&str]) -> Result<Lock, PipError> {
        let mut lock = locked(requirements);
        lock_pip(&mut lock, |url| self.fetch(url))?;
        Ok(lock)
    }

    fn modify(&self, name: &str, edit: impl FnOnce(&mut Value)) {
        let path = self.root.path().join(format!("simple/{name}/index.json"));
        let mut project = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut project);
        std::fs::write(path, project.to_string()).unwrap();
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn package(name: &str, version: &str) -> Package {
    Package {
        name: name.into(),
        version: version.into(),
        build: "0".into(),
        channel: "fixture".into(),
        subdir: "linux-64".into(),
        file: format!("{name}-{version}-0.conda"),
        url: format!("https://conda.example/{name}-{version}-0.conda"),
        sha256: Some("a".repeat(64)),
        md5: None,
    }
}

fn locked(requirements: &[&str]) -> Lock {
    Lock {
        format: 1,
        platform: "linux-64".into(),
        channels: vec![],
        specs: vec!["python".into(), "pip".into()],
        pip: requirements.iter().map(|text| (*text).into()).collect(),
        packages: vec![package("python", "3.12.8"), package("pip", "24.3.1")],
        virtual_packages: Some(vec![VirtualPackage {
            name: "__glibc".into(),
            version: "2.28".into(),
            build: "0".into(),
        }]),
        pypi: None,
    }
}

#[test]
fn locks_dependency_closure() {
    let index = Index::new();
    index.add("alpha-1-py3-none-any.whl", "Metadata-Version: 2.3\nName: alpha\nVersion: 1\nProvides-Extra: feature\nRequires-Dist: beta>=1\nRequires-Dist: extra==1; extra == 'feature'\nRequires-Dist: missing; sys_platform == 'win32'\n");
    index.add("beta-1-py3-none-any.whl", "Name: beta\nVersion: 1\n");
    index.add(
        "beta-2-py3-none-any.whl",
        "Name: beta\nVersion: 2\nRequires-Python: >=3.13\n",
    );
    index.add(
        "extra-1-cp312-cp312-manylinux_2_28_x86_64.whl",
        "Name: extra\nVersion: 1\n",
    );
    let lock = index.resolve(&["alpha[feature]==1"]).unwrap();
    let pins = lock.pypi.as_ref().unwrap();
    assert_eq!(pins.python, "3.12.8");
    assert_eq!(
        pins.packages
            .iter()
            .map(|package| (package.name.as_str(), package.version.as_str()))
            .collect::<Vec<_>>(),
        [("alpha", "1"), ("beta", "1"), ("extra", "1")]
    );
    for package in &pins.packages {
        assert_eq!(package.sha256, hash(&index.fetch(&package.url).unwrap()));
    }
    assert_eq!(
        index.resolve(&["alpha[feature]==1"]).unwrap().digest(),
        lock.digest()
    );
}

#[test]
fn backtracks_transitive_conflicts() {
    let index = Index::new();
    for (version, dependency) in [("1", "beta==2"), ("2", "beta<2")] {
        index.add(
            &format!("alpha-{version}-py3-none-any.whl"),
            &format!("Name: alpha\nVersion: {version}\nRequires-Dist: {dependency}\n"),
        );
    }
    for version in ["1", "2"] {
        index.add(
            &format!("beta-{version}-py3-none-any.whl"),
            &format!("Name: beta\nVersion: {version}\nRequires-Dist: alpha>=1\n"),
        );
    }
    let lock = index.resolve(&["alpha", "beta==2"]).unwrap();
    assert_eq!(lock.pypi.unwrap().packages[0].version, "1");
    assert!(matches!(
        index.resolve(&["alpha==2", "beta==2"]),
        Err(PipError::Resolution(_))
    ));
}

#[test]
fn rejects_unsupported_inputs() {
    for text in [
        "-e .",
        "./local",
        "/tmp/local",
        "git+https://example.org/project",
        "alpha @ https://example.org/a.whl",
        "--index-url https://example.org",
        "--extra-index-url https://example.org",
        "alpha; platform_release == '1'",
        "alpha\n--index-url other",
    ] {
        assert!(
            matches!(validate_pip(&[text.into()]), Err(PipError::Unsupported(_))),
            "{text}"
        );
    }
    validate_pip(&["alpha[feature]>=1; python_version >= '3.10'".into()]).unwrap();
}

#[test]
fn checks_target_compatibility() {
    let index = Index::new();
    for (version, tag) in [
        ("1", "cp311-abi3-manylinux2014_x86_64"),
        ("2", "cp313-cp313-manylinux_2_28_x86_64"),
        ("3", "cp312-cp312-manylinux_2_28_aarch64"),
        ("4", "cp312-cp312-manylinux_2_35_x86_64"),
    ] {
        index.add(
            &format!("alpha-{version}-{tag}.whl"),
            &format!("Name: alpha\nVersion: {version}\n"),
        );
    }
    assert_eq!(
        index.resolve(&["alpha"]).unwrap().pypi.unwrap().packages[0].version,
        "1"
    );
    let mut lock = locked(&["alpha"]);
    lock.virtual_packages = None;
    assert!(lock_pip(&mut lock, |url| index.fetch(url)).is_err());
    lock.packages.retain(|package| package.name != "python");
    assert!(matches!(
        lock_pip(&mut lock, |_| panic!("no network without Python")),
        Err(PipError::Unsupported(_))
    ));
}

#[test]
fn verifies_metadata_hashes() {
    let index = Index::new();
    index.add("alpha-1-py3-none-any.whl", "Name: alpha\nVersion: 1\n");
    index.modify("alpha", |project| {
        project["files"][0]["core-metadata"] = json!(false);
    });
    index.resolve(&["alpha"]).unwrap();
    index.modify("alpha", |project| {
        project["files"][0]["hashes"]["sha256"] = json!("b".repeat(64));
    });
    assert!(
        index
            .resolve(&["alpha"])
            .unwrap_err()
            .to_string()
            .contains("SHA-256 mismatch")
    );
    index.modify("alpha", |project| {
        project["files"][0]["hashes"] = json!({});
    });
    assert!(matches!(
        index.resolve(&["alpha"]),
        Err(PipError::Unsupported(_))
    ));
    index.modify("alpha", |project| {
        project["files"][0]["hashes"]["sha256"] = json!("a".repeat(64));
        project["files"][0]["core-metadata"] = json!({"sha256":"b".repeat(64)});
    });
    let mut lock = locked(&["alpha"]);
    assert!(
        lock_pip(&mut lock, |url| index.fetch(url))
            .unwrap_err()
            .to_string()
            .contains("SHA-256 mismatch")
    );
    assert!(lock.pypi.is_none());
}

#[test]
fn requires_source_tools() {
    let index = Index::new();
    index.add(
        "alpha-1.tar.gz",
        "Metadata-Version: 2.3\nName: alpha\nVersion: 1\n",
    );
    assert!(
        index
            .resolve(&["alpha"])
            .unwrap_err()
            .to_string()
            .contains("build tools")
    );
    let mut lock = locked(&["alpha"]);
    lock.packages
        .extend([package("setuptools", "75"), package("wheel", "0.45")]);
    lock_pip(&mut lock, |url| index.fetch(url)).unwrap();
    assert!(lock.pypi.unwrap().packages[0].file.ends_with(".tar.gz"));
    index.add(
        "alpha-2.tar.gz",
        "Metadata-Version: 2.3\nName: alpha\nVersion: 2\nDynamic: Requires-Dist\n",
    );
    lock.pypi = None;
    assert!(
        lock_pip(&mut lock, |url| index.fetch(url))
            .unwrap_err()
            .to_string()
            .contains("dynamic source metadata")
    );
}
