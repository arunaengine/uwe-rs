//! Tests package selection, channel priority, target capabilities, and lock failures.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use conda_environment::{Environment, Index, LockError, VirtualPackage, lock, lock_with_virtual};
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
        "md5": "a".repeat(32),
        "sha256": "b".repeat(64),
    });
    (file, record)
}

fn write(folder: &Path, channel: &str, subdir: &str, packages: Vec<(String, Value)>) {
    let packages: serde_json::Map<String, Value> = packages
        .into_iter()
        .map(|(file, mut record)| {
            record["subdir"] = json!(subdir);
            (file, record)
        })
        .collect();
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
    let capabilities = [VirtualPackage {
        name: "__glibc".into(),
        version: "2.17".into(),
        build: "0".into(),
    }];
    let lock = lock_with_virtual(&environment, folder, "linux-64", &capabilities)?;
    Ok((lock.packages.iter())
        .map(|package| format!("{}::{}={}", package.channel, package.name, package.version))
        .collect())
}

const CHANNELS: &str = "channels:\n  - conda-forge\n  - bioconda\n";

#[test]
fn rejects_repodata_escape() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    for channel in ["../outside", "/outside", "pkgs//main"] {
        let error = Index::load(folder.path(), &[channel.to_owned()], "linux-64")
            .err()
            .expect("an invalid channel path");
        assert!(
            matches!(
                error,
                LockError::Io { ref error, .. }
                    if error.kind() == std::io::ErrorKind::InvalidInput
            ),
            "{error}"
        );
    }
    for platform in ["../outside", "/linux-64", "linux/amd64"] {
        let error = Index::load(folder.path(), &["conda-forge".to_owned()], platform)
            .err()
            .expect("an invalid platform path");
        assert!(
            matches!(
                error,
                LockError::Io { ref error, .. }
                    if error.kind() == std::io::ErrorKind::InvalidInput
            ),
            "{error}"
        );
    }
}

#[test]
fn accepts_compound_channels() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    write(
        folder.path(),
        "pkgs/main",
        "linux-64",
        vec![package("python", "3.13", "0", &[])],
    );
    write(folder.path(), "pkgs/main", "noarch", vec![]);
    Index::load(folder.path(), &["pkgs/main".to_owned()], "linux-64")
        .expect("compound channel path");
}

#[test]
fn selects_newest_packages() {
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
fn binds_package_digests() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let environment =
        Environment::parse(&format!("{CHANNELS}dependencies:\n  - libzlib\n")).expect("parses");
    let capabilities = [VirtualPackage {
        name: "__glibc".into(),
        version: "2.17".into(),
        build: "0".into(),
    }];
    let lock =
        lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities).expect("a lock");
    let package = &lock.packages[0];
    assert_eq!(
        package.url,
        "https://conda.anaconda.org/conda-forge/linux-64/libzlib-1.3.1-hb9d3cd8_2.tar.bz2"
    );
    assert_eq!(package.sha256.as_deref(), Some("b".repeat(64).as_str()));
    assert_eq!(
        lock.explicit(),
        "# platform: linux-64\n@EXPLICIT\nhttps://conda.anaconda.org/conda-forge/linux-64/\
         libzlib-1.3.1-hb9d3cd8_2.tar.bz2#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
    );
    let again =
        lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities).expect("a lock");
    assert_eq!(lock.digest(), again.digest());
    assert!(lock.digest().starts_with("sha256:"));
}

#[test]
fn explains_lock_failures() {
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

#[test]
fn enforces_bracket_constraints() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let mut user = package("numpy-user", "1.0", "0", &["numpy"]);
    user.1["constrains"] = json!(["numpy[version='>=1.2']"]);
    write(
        folder.path(),
        "conda-forge",
        "linux-64",
        vec![
            user,
            package("numpy", "1.2", "0", &[]),
            package("numpy", "1.1", "0", &[]),
        ],
    );
    write(folder.path(), "conda-forge", "noarch", vec![]);
    for dependencies in ["  - numpy-user\n", "  - numpy\n  - numpy-user\n"] {
        let packages = locked(
            folder.path(),
            &format!("channels:\n  - conda-forge\ndependencies:\n{dependencies}"),
        )
        .expect("a bracket constraint is supported");
        assert!(packages.contains(&"conda-forge::numpy=1.2".into()));
        assert!(
            locked(
                folder.path(),
                "channels: [conda-forge]\ndependencies: [numpy-user, numpy=1.1]\n"
            )
            .is_err()
        );
    }
}

#[test]
fn excludes_negated_versions() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let mut user = package("numpy-user", "1.0", "0", &["numpy"]);
    user.1["constrains"] = json!(["numpy !=1.2.*"]);
    write(folder.path(), "conda-forge", "noarch", vec![]);
    for with_fallback in [false, true] {
        let mut packages = vec![user.clone(), package("numpy", "1.2", "0", &[])];
        if with_fallback {
            packages.push(package("numpy", "1.1", "0", &[]));
        }
        write(folder.path(), "conda-forge", "linux-64", packages);
        for dependencies in ["  - numpy-user\n", "  - numpy\n  - numpy-user\n"] {
            let result = locked(
                folder.path(),
                &format!("channels:\n  - conda-forge\ndependencies:\n{dependencies}"),
            );
            if with_fallback {
                assert!(
                    result
                        .expect("an allowed older version")
                        .contains(&"conda-forge::numpy=1.1".into())
                );
            } else {
                let error = result.expect_err("the only version is excluded");
                assert!(matches!(error, LockError::Conflict { .. }), "{error}");
            }
        }
    }
}

#[test]
fn enforces_channel_requirements() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let mut user = package("foo-user", "1.0", "0", &["foo"]);
    user.1["constrains"] = json!(["B::foo"]);
    write(
        folder.path(),
        "A",
        "linux-64",
        vec![package("foo", "1.0", "0", &[]), user],
    );
    write(folder.path(), "A", "noarch", vec![]);
    write(
        folder.path(),
        "B",
        "linux-64",
        vec![package("foo", "1.0", "0", &[])],
    );
    write(folder.path(), "B", "noarch", vec![]);
    for dependencies in [
        "  - A::foo\n  - B::foo\n",
        "  - B::foo\n  - A::foo\n",
        "  - foo-user\n  - A::foo\n",
        "  - A::foo\n  - foo-user\n",
    ] {
        let error = locked(
            folder.path(),
            &format!("channels:\n  - A\n  - B\ndependencies:\n{dependencies}"),
        )
        .expect_err("the channel requirements conflict");
        assert!(matches!(error, LockError::Conflict { .. }), "{error}");
    }
}

#[test]
fn backtracks_virtual_dependencies() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let environment = Environment::parse(&format!("{CHANNELS}dependencies:\n  - libzlib\n"))
        .expect("an environment");
    for (version, expected) in [("2.16", "1.2.13"), ("2.17", "1.3.1")] {
        let capabilities = [VirtualPackage {
            name: "__glibc".into(),
            version: version.into(),
            build: "0".into(),
        }];
        let locked = lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities)
            .expect("a compatible lock");
        assert_eq!(locked.packages.len(), 1);
        assert_eq!(locked.packages[0].version, expected);
        assert_eq!(
            locked.virtual_packages.as_deref(),
            Some(capabilities.as_slice())
        );
        assert!(!locked.explicit().contains("__glibc"));
    }
    let environment = Environment::parse(&format!("{CHANNELS}dependencies:\n  - libzlib=1.3.1\n"))
        .expect("an environment");
    assert!(matches!(
        lock_with_virtual(&environment, folder.path(), "linux-64", &[]),
        Err(LockError::Conflict { .. })
    ));
    assert!(lock(&environment, folder.path(), "linux-64").is_err());
}

#[test]
fn enforces_virtual_capabilities() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    write(
        folder.path(),
        "A",
        "linux-64",
        vec![package(
            "gpu",
            "1.0",
            "0",
            &["__cuda >=12", "__archspec 1 x86_64_v3"],
        )],
    );
    write(folder.path(), "A", "noarch", vec![]);
    let environment =
        Environment::parse("channels:\n  - A\ndependencies:\n  - gpu\n").expect("an environment");
    let mut capabilities = vec![
        VirtualPackage {
            name: "__archspec".into(),
            version: "1".into(),
            build: "x86_64_v3".into(),
        },
        VirtualPackage {
            name: "__cuda".into(),
            version: "12.2".into(),
            build: "0".into(),
        },
    ];
    assert!(lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities).is_ok());
    capabilities[0].build = "x86_64".into();
    assert!(matches!(
        lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities),
        Err(LockError::Conflict { .. })
    ));
    capabilities[0].build = "x86_64_v3".into();
    capabilities[1].version = "11.8".into();
    assert!(lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities).is_err());
    capabilities.pop();
    assert!(lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities).is_err());
    let direct =
        Environment::parse("channels:\n  - A\ndependencies:\n  - __archspec=1=x86_64_v3\n")
            .expect("an environment");
    assert!(lock_with_virtual(&direct, folder.path(), "linux-64", &capabilities).is_ok());
    assert!(matches!(
        lock_with_virtual(&direct, folder.path(), "linux-64", &[]),
        Err(LockError::NoPackage { .. })
    ));
}

#[test]
fn checks_provided_capabilities() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let mut newer = package("user", "2.0", "0", &[]);
    newer.1["constrains"] = json!(["__glibc >=2.28", "__cuda >=12"]);
    let mut older = package("user", "1.0", "0", &[]);
    older.1["constrains"] = json!(["__glibc <2.28", "__cuda >=12"]);
    write(folder.path(), "A", "linux-64", vec![newer, older]);
    write(folder.path(), "A", "noarch", vec![]);
    let environment =
        Environment::parse("channels:\n  - A\ndependencies:\n  - user\n").expect("an environment");
    let mut capabilities = vec![VirtualPackage {
        name: "__glibc".into(),
        version: "2.17".into(),
        build: "0".into(),
    }];
    let locked = lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities)
        .expect("CUDA constraints do not require CUDA");
    assert_eq!(locked.packages[0].version, "1.0");
    capabilities.push(VirtualPackage {
        name: "__cuda".into(),
        version: "11.8".into(),
        build: "0".into(),
    });
    assert!(matches!(
        lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities),
        Err(LockError::Conflict { .. })
    ));
    let locked = lock_with_virtual(&environment, folder.path(), "linux-64", &[])
        .expect("absent capabilities are not required by constrains");
    assert_eq!(locked.packages[0].version, "2.0");
}

#[test]
fn distinguishes_capability_identity() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    channels(folder.path());
    let environment = Environment::parse(&format!("{CHANNELS}dependencies:\n  - pinned\n"))
        .expect("an environment");
    let legacy = lock(&environment, folder.path(), "linux-64").expect("a legacy lock");
    let serialized = serde_json::to_value(&legacy).expect("serialized lock");
    assert!(serialized.get("virtual_packages").is_none());
    let restored: conda_environment::Lock = serde_json::from_value(serialized).expect("old JSON");
    assert_eq!(restored, legacy);
    let absent = lock_with_virtual(&environment, folder.path(), "linux-64", &[])
        .expect("authoritative absence");
    assert_ne!(legacy.digest(), absent.digest());
    let mut capabilities = vec![
        VirtualPackage {
            name: "__glibc".into(),
            version: "2.17".into(),
            build: "0".into(),
        },
        VirtualPackage {
            name: "__archspec".into(),
            version: "1".into(),
            build: "x86_64".into(),
        },
    ];
    let first = lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities)
        .expect("capability lock");
    capabilities.reverse();
    let reordered = lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities)
        .expect("reordered capability lock");
    assert_eq!(first.digest(), reordered.digest());
    capabilities[1].version = "2.28".into();
    let changed = lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities)
        .expect("changed capability lock");
    assert_ne!(first.digest(), changed.digest());
    capabilities.push(capabilities[0].clone());
    assert!(matches!(
        lock_with_virtual(&environment, folder.path(), "linux-64", &capabilities),
        Err(LockError::Spec(_))
    ));
}

#[test]
fn permits_lower_channels() {
    let folder = tempfile::tempdir().unwrap();
    write(
        folder.path(),
        "A",
        "linux-64",
        vec![package("foo", "2", "0", &["missing"])],
    );
    write(
        folder.path(),
        "B",
        "linux-64",
        vec![package("foo", "1", "0", &[])],
    );
    for channel in ["A", "B"] {
        write(folder.path(), channel, "noarch", vec![]);
    }
    assert_eq!(
        locked(folder.path(), "channels: [A, B]\ndependencies: [foo]\n").unwrap(),
        ["B::foo=1"]
    );
    assert!(locked(folder.path(), "channels: [A, B]\ndependencies: [A::foo]\n").is_err());
}

#[test]
fn rejects_pip_requirements() {
    let environment =
        Environment::parse("dependencies:\n  - pip:\n    - requests==2.32\n").unwrap();
    assert!(matches!(
        lock(&environment, Path::new("missing"), "linux-64"),
        Err(LockError::Pip)
    ));
}

#[test]
fn enforces_package_location() {
    let folder = tempfile::tempdir().unwrap();
    write(
        folder.path(),
        "A",
        "linux-64",
        vec![package("foo", "2", "0", &[])],
    );
    write(
        folder.path(),
        "A",
        "noarch",
        vec![package("foo", "1", "0", &[])],
    );
    for spec in ["A/noarch::foo", "foo[fn='foo-1-0.tar.bz2']"] {
        let environment =
            Environment::parse(&format!("channels: [A]\ndependencies:\n  - {spec}\n")).unwrap();
        let locked = lock(&environment, folder.path(), "linux-64").unwrap();
        assert_eq!(locked.packages[0].version, "1");
        assert_eq!(locked.packages[0].subdir, "noarch");
    }
}
