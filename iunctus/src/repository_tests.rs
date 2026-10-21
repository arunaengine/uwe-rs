//! Checks repository identities, manifest branches and safe archive extraction.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn archive(files: &[(&str, &str)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    let metadata = "52 comment=0123456789abcdef0123456789abcdef01234567\n";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::XGlobalHeader);
    header.set_size(metadata.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "pax_global_header", metadata.as_bytes())
        .unwrap();
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
        (
            "https://git.example.org/team/pipeline.git",
            Host::Git,
            "https://git.example.org/team/pipeline.git",
        ),
        (
            "https://git.example.org/group/team/pipeline",
            Host::Git,
            "https://git.example.org/group/team/pipeline",
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
fn asks_for_git() {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            // Only the repository answers as Git; the plain file is not found below it.
            let answer = if line.contains("/team/tools/pipeline/info/refs") {
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 0\r\n\r\n"
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
            };
            stream.write_all(answer.as_bytes()).unwrap();
        }
    });
    let runtime = workflow_graph::runtime().unwrap();
    let git = format!("http://{address}/team/tools/pipeline");
    let file = format!("http://{address}/team/tools/Snakefile");
    assert!(runtime.block_on(speaks_git(&git, None)));
    assert!(!runtime.block_on(speaks_git(&file, None)));
    server.join().unwrap();
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

#[test]
#[ignore = "fetches https://github.com/nextflow-io/hello.git over the network"]
fn fetches_plain_git() {
    let cache = tempfile::tempdir().unwrap();
    let mut repository = Repository::parse("https://github.com/nextflow-io/hello.git")
        .unwrap()
        .unwrap();
    repository.host = Host::Git;
    let folder = repository
        .fetch_git(cache.path(), Some("master"), false, None)
        .unwrap();
    assert!(folder.join("main.nf").is_file(), "{folder:?}");
    let commit = std::fs::read_to_string(folder.join(MARKER)).unwrap();
    let again = repository
        .fetch_git(cache.path(), Some(&commit), false, None)
        .unwrap();
    assert_eq!(again, folder);
}
