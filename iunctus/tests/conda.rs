//! `iunctus lock-environment` locks a conda environment file with the repodata in the cache
//! folder, without network, and says what is missing when the folder lacks repodata.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// The repodata of one small channel for linux-64, and its empty `noarch`.
fn repodata(cache: &Path) {
    let folder = cache.join("iunctus/conda/conda-forge");
    let packages = serde_json::json!({
        "packages": {
            "libzlib-1.3.1-hb9d3cd8_2.tar.bz2": {
                "name": "libzlib", "version": "1.3.1", "build": "hb9d3cd8_2",
                "depends": ["__glibc >=2.17"], "md5": "0123", "sha256": "abcd"
            },
            "zlib-1.3.1-hb9d3cd8_2.tar.bz2": {
                "name": "zlib", "version": "1.3.1", "build": "hb9d3cd8_2",
                "depends": ["libzlib 1.3.1 hb9d3cd8_2"], "md5": "4567", "sha256": "ef01"
            }
        }
    });
    for (subdir, text) in [("linux-64", packages.to_string()), ("noarch", "{}".into())] {
        std::fs::create_dir_all(folder.join(subdir)).expect("makes the folder");
        std::fs::write(folder.join(subdir).join("repodata.json"), text).expect("writes");
    }
}

fn lock(folder: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .arg("lock-environment")
        .args(arguments)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .output()
        .expect("runs iunctus");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn an_environment_file_gets_its_lock_from_the_repodata_in_the_cache() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("env.yaml"), "dependencies:\n  - zlib=1.3\n").expect("writes");
    // Without repodata, the machine is not ready, and the error says where it belongs.
    let (code, _, error) = lock(root, &["env.yaml", "-platform", "linux-64"]);
    assert_eq!(code, 3, "{error}");
    assert!(
        error.contains("https://conda.anaconda.org/conda-forge/linux-64/repodata.json"),
        "{error}"
    );
    repodata(&root.join("cache"));
    let (code, output, error) = lock(root, &["env.yaml", "-platform", "linux-64"]);
    assert_eq!(code, 0, "{error}");
    let locked: serde_json::Value = serde_json::from_str(&output).expect("a lock as JSON");
    let names: Vec<&str> = (locked["packages"].as_array().expect("packages").iter())
        .filter_map(|package| package["name"].as_str())
        .collect();
    assert_eq!(names, ["libzlib", "zlib"]);
    let (code, output, error) = lock(
        root,
        &["env.yaml", "-platform", "linux-64", "-output", "env.txt"],
    );
    assert_eq!(code, 0, "{error}");
    assert!(
        output.contains("locked 2 packages for linux-64"),
        "{output}"
    );
    let explicit = std::fs::read_to_string(root.join("env.txt")).expect("reads");
    assert!(
        explicit.starts_with("# platform: linux-64\n@EXPLICIT\n"),
        "{explicit}"
    );
    assert!(
        explicit.contains("conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.tar.bz2#4567"),
        "{explicit}"
    );
    // A package that no channel has is the environment's mistake.
    std::fs::write(root.join("env.yaml"), "dependencies:\n  - zlib=2\n").expect("writes");
    let (code, _, error) = lock(root, &["env.yaml", "-platform", "linux-64"]);
    assert_eq!(code, 1, "{error}");
    assert!(
        error.contains("no package of the channels conda-forge fits `zlib=2`"),
        "{error}"
    );
    let (code, _, error) = lock(root, &[]);
    assert_eq!(code, 2, "{error}");
}
