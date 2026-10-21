//! `iunctus lock-environment` locks a conda environment file with the repodata in the cache
//! folder, without network, and says what is missing when the folder lacks repodata.
// Copyright (c) 2026 The uwe Contributors
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
                "build_number":0, "subdir":"linux-64", "depends": [], "md5": "a".repeat(32), "sha256": "b".repeat(64)
            },
            "zlib-1.3.1-hb9d3cd8_2.tar.bz2": {
                "name": "zlib", "version": "1.3.1", "build": "hb9d3cd8_2",
                "build_number":0, "subdir":"linux-64", "depends": ["libzlib 1.3.1 hb9d3cd8_2"], "md5": "c".repeat(32), "sha256": "d".repeat(64)
            }
        }
    });
    for (subdir, text) in [("linux-64", packages.to_string()), ("noarch", "{}".into())] {
        std::fs::create_dir_all(folder.join(subdir)).expect("makes the folder");
        std::fs::write(folder.join(subdir).join("repodata.json"), text).expect("writes");
    }
}

fn lock(folder: &Path, arguments: &[&str]) -> (i32, String, String) {
    let config = folder.join("offline.toml");
    std::fs::write(&config, "[network]\ndefault = 'none'\n").unwrap();
    let output = common::command()
        .arg("lock-environment")
        .arg("-iunctus-config")
        .arg(&config)
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
fn locks_cached_metadata() {
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
        explicit.contains(
            "conda-forge/linux-64/zlib-1.3.1-hb9d3cd8_2.tar.bz2#cccccccccccccccccccccccccccccccc"
        ),
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

fn configuration(
    root: &Path,
    base: &str,
    capabilities: &Path,
    repodata: &Path,
    packages: &Path,
) -> std::path::PathBuf {
    let file = root.join("conda.toml");
    let quote = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    std::fs::write(&file, format!(
        "[conda]\nbase = {base:?}\nplatform = \"linux/amd64\"\ninstaller = \"micromamba\"\noffline = true\ncapabilities = {}\nrepodata = {}\npackages = {}\n",
        quote(capabilities), quote(repodata), quote(packages),
    )).unwrap();
    file
}

#[test]
fn rejects_unbound_capabilities() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let base = format!("mamba@sha256:{}", "a".repeat(64));
    let capabilities = root.join("target.json");
    std::fs::write(
        &capabilities,
        serde_json::json!({"base":base,"platform":"linux/amd64","virtual_packages":[]}).to_string(),
    )
    .unwrap();
    let config = configuration(
        root,
        &base,
        &capabilities,
        &root.join("repodata"),
        &root.join("packages"),
    );
    let original = std::fs::read_to_string(&config).unwrap();
    std::fs::write(root.join("main.nf"), "workflow {}\n").unwrap();
    for (from, to, expected) in [
        (
            "installer = \"micromamba\"",
            "installer = \"conda\"",
            "inside the pinned base",
        ),
        (
            "platform = \"linux/amd64\"",
            "platform = \"linux/arm64\"",
            "exactly this base and platform",
        ),
        ("[conda]", "[conda]\nunknown = true", "unknown"),
    ] {
        std::fs::write(&config, original.replace(from, to)).unwrap();
        let output = common::command()
            .args(["graph", "main.nf", "-iunctus-config"])
            .arg(&config)
            .current_dir(root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(3), "{error}");
        assert!(error.contains(expected), "{error}");
    }
    std::fs::write(
        &config,
        original.replace("offline = true", "offline = false"),
    )
    .unwrap();
    let output = common::command()
        .args(["graph", "main.nf", "-iunctus-config"])
        .arg(&config)
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn validates_target_capabilities() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let cache = root.join("cache");
    repodata(&cache);
    let repodata = cache.join("iunctus/conda");
    let path = repodata.join("conda-forge/linux-64/repodata.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    metadata["packages"]["libzlib-1.3.1-hb9d3cd8_2.tar.bz2"]["depends"] =
        serde_json::json!(["__glibc >=2.17"]);
    std::fs::write(path, metadata.to_string()).unwrap();
    let base = format!("mamba@sha256:{}", "a".repeat(64));
    let capabilities = root.join("target.json");
    let config = configuration(
        root,
        &base,
        &capabilities,
        &repodata,
        &root.join("packages"),
    );
    std::fs::write(root.join("env.yaml"), "dependencies: [zlib]\n").unwrap();
    for (version, code) in [("2.17", 0), ("2.16", 1)] {
        std::fs::write(
            &capabilities,
            serde_json::json!({"base":base,"platform":"linux/amd64",
            "virtual_packages":[{"name":"__glibc","version":version,"build":"0"}]})
            .to_string(),
        )
        .unwrap();
        let (actual, output, error) = lock(
            root,
            &["env.yaml", "-iunctus-config", config.to_str().unwrap()],
        );
        assert_eq!(actual, code, "{error}");
        if code == 0 {
            let lock: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(lock["virtual_packages"][0]["version"], version);
        }
    }
    std::fs::write(
        root.join("env.yaml"),
        "dependencies:\n  - pip:\n    - '-e .'\n",
    )
    .unwrap();
    let (code, _, error) = lock(
        root,
        &["env.yaml", "-iunctus-config", config.to_str().unwrap()],
    );
    assert_eq!(code, 33);
    assert!(error.contains("pip"), "{error}");
}

#[test]
fn rejects_pip_forms() {
    let folder = tempfile::tempdir().unwrap();
    for requirement in [
        "-e .",
        "./local",
        "local.whl",
        "/tmp/local",
        "git+https://example.org/project",
        "alpha @ https://example.org/a.whl",
        "--index-url https://example.org",
        "--extra-index-url https://example.org",
    ] {
        std::fs::write(
            folder.path().join("env.yaml"),
            format!("dependencies:\n  - pip:\n    - {requirement:?}\n"),
        )
        .unwrap();
        let (code, _, error) = lock(folder.path(), &["env.yaml", "-platform", "linux-64"]);
        assert_eq!(code, 33, "{requirement}: {error}");
        assert!(error.contains("pip"), "{error}");
    }
}

#[test]
#[ignore = "requires Docker/Podman, the pinned micromamba base, and IUNCTUS_CONDA_TEST_FIXTURE with target.json, repodata and SHA-256 packages"]
fn reuses_prepared_image() {
    let fixture = std::path::PathBuf::from(
        std::env::var_os("IUNCTUS_CONDA_TEST_FIXTURE").expect("explicit offline fixture"),
    );
    let target = fixture.join("target.json");
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
    let base = metadata["base"].as_str().unwrap();
    assert_eq!(metadata["platform"], "linux/amd64");
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let packages = root.join("packages");
    std::fs::create_dir(&packages).unwrap();
    let digest = "b928c30ddcb0e3f544c6eade8352737e6e610e263276b90232db6a578ef899d8";
    std::fs::copy(fixture.join("packages").join(digest), packages.join(digest)).unwrap();
    let repodata = fixture.join("repodata");
    let config = configuration(root, base, &target, &repodata, &packages);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let environment = format!("dependencies:\n  - tzdata=2026c\n  - tzdata!=0.{nonce}\n");
    let capabilities: Vec<conda_environment::VirtualPackage> =
        serde_json::from_value(metadata["virtual_packages"].clone()).unwrap();
    let mut parsed = conda_environment::Environment::parse(&environment).unwrap();
    parsed.channels.push("conda-forge".into());
    let lock = conda_environment::lock_with_virtual(&parsed, &repodata, "linux-64", &capabilities)
        .unwrap();
    let preparation =
        conda_environment::ImagePreparation::new(&lock, base, "linux/amd64", &capabilities)
            .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let store = content_store::Store::local(&root.join("store")).unwrap();
    let mut first = None;
    for name in ["first", "second"] {
        let work = root.join(name);
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("env.yaml"), &environment).unwrap();
        std::fs::write(work.join("Snakefile"), "rule result:\n    output: 'timezone.txt'\n    conda: 'env.yaml'\n    shell: \"test -s /opt/iunctus/share/zoneinfo/UTC && printf timezone-ready > {output}\"\n").unwrap();
        let output = common::command()
            .args([
                "run",
                "Snakefile",
                "-executor",
                "local",
                "-no-cache",
                "-no-network",
                "-iunctus-config",
            ])
            .arg(&config)
            .arg("-store")
            .arg(root.join("store"))
            .current_dir(&work)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(work.join("timezone.txt")).unwrap(),
            "timezone-ready"
        );
        let cache = conda_environment::ImageCache::new(store.clone());
        let found = runtime
            .block_on(cache.lookup(&preparation))
            .unwrap()
            .unwrap();
        match &first {
            None => {
                first = Some(found.receipt().clone());
                std::fs::remove_dir_all(&packages).unwrap();
            }
            Some(first) => assert_eq!(first, found.receipt()),
        }
    }
}
