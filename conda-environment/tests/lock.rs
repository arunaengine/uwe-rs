//! Locks environments with the repodata of two small channels in a folder: the newest packages
//! that fit, the order of channels, going back from a choice that cannot fit, constraints of
//! packages, and the errors of missing repodata and packages.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use conda_environment::{Environment, LockError, lock};
use serde_json::{Value, json};

/// A package of a repodata file.
fn package(name: &str, version: &str, build: &str, depends: &[&str]) -> (String, Value) {
    let file = format!("{name}-{version}-{build}.tar.bz2");
    let record = json!({
        "name": name,
        "version": version,
        "build": build,
        "build_number": 0,
        "depends": depends,
        "md5": format!("md5-of-{name}-{version}"),
        "sha256": format!("sha256-of-{name}-{version}"),
    });
    (file, record)
}

fn write(folder: &Path, channel: &str, subdir: &str, packages: Vec<(String, Value)>) {
    let packages: serde_json::Map<String, Value> = packages.into_iter().collect();
    let path = folder.join(channel).join(subdir);
    std::fs::create_dir_all(&path).expect("makes the folder");
    let repodata = json!({"info": {"subdir": subdir}, "packages": packages});
    std::fs::write(path.join("repodata.json"), repodata.to_string()).expect("writes");
}

/// conda-forge has zlib and a newer and an older build of a library; bioconda has samtools
/// and htslib in two versions each, and an older zlib, which conda-forge wins over.
fn channels(folder: &Path) {
    write(
        folder,
        "conda-forge",
        "linux-64",
        vec![
            package("libzlib", "1.3.1", "hb9d3cd8_2", &["__glibc >=2.17"]),
            package("libzlib", "1.2.13", "h4ab18f5_6", &[]),
            package("pinned", "1.0", "0", &[]),
        ],
    );
    write(folder, "conda-forge", "noarch", vec![]);
    let mut old_only = package("oldzlib-user", "1.0", "0", &["libzlib"]);
    old_only.1["constrains"] = json!(["libzlib <1.3"]);
    write(
        folder,
        "bioconda",
        "linux-64",
        vec![
            package(
                "samtools",
                "1.18",
                "h50ea8bc_1",
                &["htslib >=1.18,<1.19.0a0"],
            ),
            package(
                "samtools",
                "1.17",
                "hd87286a_2",
                &["htslib >=1.17,<1.18.0a0", "libzlib"],
            ),
            package("htslib", "1.18", "h81da01d_0", &["libzlib >=1.2.13"]),
            package("htslib", "1.17", "h6bc39ce_1", &["libzlib >=1.2.13"]),
            package("libzlib", "1.2.11", "0", &[]),
            old_only,
        ],
    );
    write(folder, "bioconda", "noarch", vec![]);
}

fn locked(folder: &Path, text: &str) -> Result<Vec<String>, LockError> {
    let environment = Environment::parse(text).expect("an environment");
    let lock = lock(&environment, folder, "linux-64")?;
    Ok((lock.packages.iter())
        .map(|package| format!("{}::{}={}", package.channel, package.name, package.version))
        .collect())
}

const CHANNELS: &str = "channels:\n  - conda-forge\n  - bioconda\n";

#[test]
fn environments_get_the_newest_packages_that_fit() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let packages = locked(
        folder.path(),
        &format!("{CHANNELS}dependencies:\n  - samtools=1.17\n"),
    )
    .expect("a lock");
    assert_eq!(
        packages,
        [
            "bioconda::htslib=1.17",
            "conda-forge::libzlib=1.3.1",
            "bioconda::samtools=1.17"
        ]
    );
    // A choice that cannot fit a later spec gives way to the next one.
    let text = format!("{CHANNELS}dependencies:\n  - samtools\n  - htslib=1.17\n");
    let packages = locked(folder.path(), &text).expect("a lock");
    assert!(
        packages.contains(&"bioconda::samtools=1.17".to_owned()),
        "{packages:?}"
    );
    // What a package constrains limits the others.
    let text = format!("{CHANNELS}dependencies:\n  - oldzlib-user\n");
    let packages = locked(folder.path(), &text).expect("a lock");
    assert!(
        packages.contains(&"conda-forge::libzlib=1.2.13".to_owned()),
        "{packages:?}"
    );
}

#[test]
fn a_lock_names_its_packages_by_address_and_digest() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let environment =
        Environment::parse(&format!("{CHANNELS}dependencies:\n  - libzlib\n")).expect("parses");
    let lock = lock(&environment, folder.path(), "linux-64").expect("a lock");
    let package = &lock.packages[0];
    assert_eq!(
        package.url,
        "https://conda.anaconda.org/conda-forge/linux-64/libzlib-1.3.1-hb9d3cd8_2.tar.bz2"
    );
    assert_eq!(package.sha256.as_deref(), Some("sha256-of-libzlib-1.3.1"));
    assert_eq!(
        lock.explicit(),
        "# platform: linux-64\n@EXPLICIT\nhttps://conda.anaconda.org/conda-forge/linux-64/\
         libzlib-1.3.1-hb9d3cd8_2.tar.bz2#md5-of-libzlib-1.3.1\n"
    );
    let again = conda_environment::lock(&environment, folder.path(), "linux-64").expect("a lock");
    assert_eq!(lock.digest(), again.digest());
    assert!(lock.digest().starts_with("sha256:"));
}

#[test]
fn what_cannot_be_locked_says_why() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let error = locked(
        folder.path(),
        &format!("{CHANNELS}dependencies:\n  - samtools=2\n"),
    )
    .expect_err("no package");
    assert!(matches!(error, LockError::NoPackage { .. }), "{error}");
    let text = format!("{CHANNELS}dependencies:\n  - samtools=1.18\n  - htslib=1.17\n");
    let error = locked(folder.path(), &text).expect_err("a conflict");
    assert!(matches!(error, LockError::Conflict { .. }), "{error}");
    let error = locked(
        folder.path(),
        "channels:\n  - r\ndependencies:\n  - r-base\n",
    )
    .expect_err("no repodata");
    let message = error.to_string();
    assert!(
        message.contains("https://conda.anaconda.org/r/linux-64/repodata.json"),
        "{message}"
    );
}
