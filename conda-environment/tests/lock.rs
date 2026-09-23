//! Locks environments with the repodata of two small channels in a folder: the newest packages
//! that fit, the order of channels, going back from a choice that cannot fit, constraints of
//! packages, and the errors of missing repodata and packages.
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
fn repodata_load_rejects_paths_outside_its_folder() {
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
fn repodata_load_accepts_compound_channel_paths() {
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

#[test]
fn bracket_package_constraints_are_enforced() {
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
fn negated_wildcard_constraints_exclude_matching_versions() {
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
fn channel_requirements_apply_to_selected_packages_and_constraints() {
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
fn virtual_glibc_versions_drive_dependency_backtracking() {
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
    assert!(lock(&environment, folder.path(), "linux-64").is_ok());
}

#[test]
fn virtual_archspec_build_and_cuda_presence_are_authoritative() {
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
fn virtual_constraints_check_provided_capabilities_without_requiring_absent_ones() {
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
fn virtual_lock_identity_distinguishes_known_capabilities_from_legacy_unknown() {
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
