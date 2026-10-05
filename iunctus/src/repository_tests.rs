//! Checks repository identities, manifest branches and safe archive extraction.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn archive(files: &[(&str, &str)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, text) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, path, text.as_bytes())
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

#[test]
fn parses_host_identities() {
    for (url, host, api) in [
        (
            "nf-core/rnaseq",
            Host::Github,
            "https://api.github.com/repos/nf-core/rnaseq",
        ),
        (
            "https://github.com/nf-core/rnaseq.git/",
            Host::Github,
            "https://api.github.com/repos/nf-core/rnaseq",
        ),
        (
            "https://gitlab.com/group/nested/workflow",
            Host::Gitlab,
            "https://gitlab.com/api/v4/projects/group%2Fnested%2Fworkflow",
        ),
        (
            "https://bitbucket.org/owner/workflow",
            Host::Bitbucket,
            "https://api.bitbucket.org/2.0/repositories/owner/workflow",
        ),
        (
            "http://localhost:1234/owner/workflow",
            Host::Gitea,
            "http://localhost:1234/api/v1/repos/owner/workflow",
        ),
    ] {
        let repository = Repository::parse(url).unwrap().unwrap();
        assert_eq!(repository.host, host);
        assert_eq!(repository.api(), api);
    }
    for local in [
        "main.nf",
        "./owner/repo",
        "../repo",
        "/owner/repo",
        "owner/repo/workflow",
        "https://example.org/tools/main.cwl",
    ] {
        assert!(Repository::parse(local).unwrap().is_none(), "{local}");
    }
    let failure = Repository::parse("https://user:secret@github.com/owner/repo")
        .err()
        .unwrap();
    assert_eq!(failure.code, 2);
    assert!(!failure.message.contains("secret"));
    assert_eq!(encode("feature/new"), "feature%2Fnew");
}

#[test]
fn extracts_workflow_files() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    let bytes = archive(&[
        ("checkout/workflow/Snakefile", "rule all:\n"),
        ("checkout/.settings", "value"),
    ]);
    unpack(bytes.as_slice(), folder.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.path().join(".settings")).unwrap(),
        "value"
    );
    assert_ne!(
        std::fs::metadata(folder.path().join("workflow/Snakefile"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        crate::detect::language(folder.path()).unwrap(),
        workflow_graph::Language::Snakemake
    );
}

#[test]
fn refuses_unsafe_archives() {
    let folder = tempfile::tempdir().unwrap();
    for files in [
        vec![("checkout/.git/HEAD", "bad")],
        vec![("checkout/.iunctus-revision", "bad")],
        vec![("first/main.nf", ""), ("other/main.nf", "")],
    ] {
        assert!(unpack(archive(&files).as_slice(), folder.path()).is_err());
    }
    let bytes = archive(&[(
        "checkout/.gitmodules",
        "[submodule \"module\"]\npath = module\n",
    )]);
    let failure = unpack(bytes.as_slice(), folder.path()).unwrap_err();
    assert_eq!(failure.code, 33);
    assert!(failure.message.contains("submodules"));
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_link_name("../../outside").unwrap();
    header.set_path("checkout/link").unwrap();
    header.set_cksum();
    archive.append(&header, std::io::empty()).unwrap();
    let bytes = archive.into_inner().unwrap().finish().unwrap();
    assert_eq!(
        unpack(bytes.as_slice(), folder.path()).unwrap_err().code,
        33
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    let path = b"checkout/../../outside";
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append(&header, std::io::empty()).unwrap();
    let bytes = archive.into_inner().unwrap().finish().unwrap();
    assert!(unpack(bytes.as_slice(), folder.path()).is_err());
    assert!(!folder.path().parent().unwrap().join("outside").exists());
}

#[test]
fn reads_manifest_branch() {
    let folder = tempfile::tempdir().unwrap();
    for text in [
        "manifest { defaultBranch = 'stable' }",
        "manifest { defaultBranch = \"stable\" }",
        "// manifest.defaultBranch = 'wrong'\nmanifest.defaultBranch = 'stable'",
    ] {
        std::fs::write(folder.path().join("nextflow.config"), text).unwrap();
        assert_eq!(
            default_branch(folder.path()).unwrap().as_deref(),
            Some("stable")
        );
    }
    std::fs::write(
        folder.path().join("nextflow.config"),
        "manifest.defaultBranch = params.branch",
    )
    .unwrap();
    assert_eq!(default_branch(folder.path()).unwrap_err().code, 33);
}

#[test]
fn records_hidden_sources() {
    let folder = tempfile::tempdir().unwrap();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    unpack(
        archive(&[
            ("checkout/main.nf", "workflow {}"),
            ("checkout/.settings", "value"),
            ("checkout/results/reference.csv", "reference"),
        ])
        .as_slice(),
        folder.path(),
    )
    .unwrap();
    std::fs::write(folder.path().join(MARKER), commit).unwrap();
    assert_eq!(
        revision::revision(&folder.path().join("main.nf")).as_deref(),
        Some(commit)
    );
    let runtime = workflow_graph::runtime().unwrap();
    let sources = runtime.block_on(crate::sources::collect(
        &folder.path().join("main.nf"),
        &crate::arguments::Arguments::default(),
    ));
    assert_eq!(sources.folder, folder.path());
    assert_eq!(
        sources
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [".settings", "main.nf", "results/reference.csv"]
    );
}

#[test]
#[ignore = "fetches nf-core/demo from GitHub"]
fn fetches_github_demo() {
    let cache = tempfile::tempdir().unwrap();
    let repository = Repository::parse("nf-core/demo").unwrap().unwrap();
    let folder = workflow_graph::runtime()
        .unwrap()
        .block_on(repository.fetch(
            cache.path(),
            Some("1.2.0"),
            false,
            repository.token().as_deref(),
        ))
        .unwrap();
    assert!(folder.join("main.nf").is_file());
    assert_eq!(
        revision::revision(&folder).as_deref(),
        Some("32893afef8076a03a2767a020b3f0cab2e0b40b2")
    );
}
