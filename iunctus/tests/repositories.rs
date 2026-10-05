//! Exercises hosted pipeline caching and pinned runs through a local archive server.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod common;

const FIRST: &str = "0123456789abcdef0123456789abcdef01234567";
const SECOND: &str = "89abcdef0123456789abcdef0123456789abcdef";
const TOKEN: &str = "fixture-private-token";

struct Server {
    address: String,
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let files = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let served = files.clone();
        let seen = requests.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let path = first.split_whitespace().nth(1).unwrap_or_default();
                seen.lock().unwrap().push(path.to_owned());
                let mut authorized = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                        break;
                    }
                    authorized |= line
                        .trim()
                        .eq_ignore_ascii_case(&format!("authorization: Bearer {TOKEN}"));
                }
                let body = served.lock().unwrap().get(path).cloned();
                let (status, body) = match (authorized, body) {
                    (true, Some(body)) => ("200 OK", body),
                    (true, None) => ("404 Not Found", Vec::new()),
                    (false, _) => ("401 Unauthorized", Vec::new()),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            address: format!("http://{address}"),
            files,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn set(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.files.lock().unwrap().insert(path.into(), body.into());
    }

    fn revision(&self, reference: &str, commit: &str, files: &[(&str, &str)]) {
        self.set(
            &format!("/api/v1/repos/owner/repo/commits/{reference}"),
            format!("{{\"sha\":\"{commit}\"}}"),
        );
        self.set(
            &format!("/api/v1/repos/owner/repo/archive/{commit}.tar.gz"),
            archive(files),
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address.trim_start_matches("http://"));
        self.thread.take().unwrap().join().unwrap();
    }
}

fn archive(files: &[(&str, &str)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, text) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("checkout/{path}"), text.as_bytes())
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn command(folder: &Path, words: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(words)
        .current_dir(folder)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .env("GITEA_TOKEN", TOKEN)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains(TOKEN));
    (output.status.code().unwrap(), text)
}

fn run(folder: &Path, url: &str, options: &[&str]) -> String {
    let mut words = vec!["run", url, "-executor", "simulator"];
    words.extend_from_slice(options);
    let (code, text) = command(folder, &words);
    assert_eq!(code, 0, "{text}");
    text
}

fn database(folder: &Path) -> run_database::Database {
    run_database::Database::open(&folder.join("cache/iunctus").join(run_database::FILE_NAME))
        .unwrap()
}

#[test]
fn refreshes_cached_branch() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::start();
    let url = format!("{}/owner/repo", server.address);
    server.set("/api/v1/repos/owner/repo", r#"{"default_branch":"main"}"#);
    server.revision(
        "main",
        FIRST,
        &[
            ("main.nf", "workflow { println 'first' }"),
            (".settings", "kept"),
        ],
    );
    assert!(run(folder.path(), &url, &[]).contains("first"));
    let first = database(folder.path()).runs(1).unwrap().remove(0);
    assert_eq!(first.revision.as_deref(), Some(FIRST));
    assert!(
        database(folder.path())
            .sources(&first.id)
            .unwrap()
            .iter()
            .any(|source| source.path == ".settings")
    );
    server.revision(
        "main",
        SECOND,
        &[("main.nf", "workflow { println 'second' }")],
    );
    assert!(run(folder.path(), &url, &[]).contains("first"));
    assert!(run(folder.path(), &url, &["-latest"]).contains("second"));
    let latest = database(folder.path()).runs(1).unwrap().remove(0);
    assert_eq!(latest.revision.as_deref(), Some(SECOND));
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.contains("/archive/"))
            .count(),
        2
    );
    let output = folder.path().join("crate");
    let (code, text) = command(
        folder.path(),
        &["export", &first.id, "-output", output.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{text}");
    let metadata = std::fs::read_to_string(output.join("ro-crate-metadata.json")).unwrap();
    assert!(metadata.contains(&format!("\"version\": \"{FIRST}\"")));
    assert!(!metadata.contains(TOKEN));
    drop(server);
    let (code, text) = command(
        folder.path(),
        &["repeat", &first.id, "-executor", "simulator"],
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("first"));
    assert_eq!(
        database(folder.path()).runs(1).unwrap()[0]
            .revision
            .as_deref(),
        Some(FIRST)
    );
    assert!(run(folder.path(), &url, &["-r", FIRST, "-latest"]).contains("first"));
    for options in [vec!["-r", "main"], vec!["-latest"], vec!["-r", &FIRST[..8]]] {
        let mut words = vec!["run", &url, "-executor", "simulator"];
        words.extend(options);
        let (code, text) = command(folder.path(), &words);
        assert_ne!(code, 0, "{text}");
        assert!(text.contains("network request failed"), "{text}");
    }
}

#[test]
fn selects_manifest_branch() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::start();
    let url = format!("{}/owner/repo", server.address);
    server.set("/api/v1/repos/owner/repo", r#"{"default_branch":"trunk"}"#);
    server.revision(
        "trunk",
        FIRST,
        &[
            ("main.nf", "workflow { println 'trunk' }"),
            ("nextflow.config", "manifest { defaultBranch = 'stable' }"),
        ],
    );
    server.revision(
        "stable",
        SECOND,
        &[("main.nf", "workflow { println 'stable' }")],
    );
    assert!(run(folder.path(), &url, &[]).contains("stable"));
    assert!(run(folder.path(), &url, &["-r", "trunk"]).contains("trunk"));
    server.revision(
        "feature%2Fnew",
        FIRST,
        &[("main.nf", "workflow { println 'trunk' }")],
    );
    assert!(run(folder.path(), &url, &["-r", "feature/new"]).contains("trunk"));
    server.revision(FIRST, FIRST, &[("main.nf", "workflow { println 'trunk' }")]);
    let fresh = tempfile::tempdir().unwrap();
    assert!(run(fresh.path(), &url, &["-r", FIRST]).contains("trunk"));
}

#[test]
fn finds_nested_snakefile() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::start();
    let url = format!("{}/owner/repo", server.address);
    server.revision(
        "v2.1.2",
        FIRST,
        &[(
            "workflow/Snakefile",
            "rule all:\n    output: 'result.txt'\n    shell: 'echo result > {output}'\n",
        )],
    );
    run(folder.path(), &url, &["-r", "v2.1.2"]);
    let record = database(folder.path()).runs(1).unwrap().remove(0);
    assert_eq!(
        record.language,
        workflow_graph::Language::Snakemake.to_string()
    );
    assert_eq!(record.revision.as_deref(), Some(FIRST));
}

#[test]
fn refuses_repository_submodules() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::start();
    let url = format!("{}/owner/repo", server.address);
    server.revision(
        "v1",
        FIRST,
        &[
            ("main.nf", "workflow {}"),
            (".gitmodules", "[submodule \"module\"]\npath = module\n"),
        ],
    );
    let (code, text) = command(
        folder.path(),
        &["run", &url, "-r", "v1", "-executor", "simulator"],
    );
    assert_eq!(code, 33, "{text}");
    assert!(text.contains("submodules"));
    assert_eq!(
        std::fs::read_dir(folder.path().join("cache/iunctus/pipelines"))
            .unwrap()
            .count(),
        1
    );
    assert!(
        !folder
            .path()
            .join("cache/iunctus")
            .join(run_database::FILE_NAME)
            .exists()
    );
}

#[test]
fn refuses_missing_token() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::start();
    let url = format!("{}/owner/repo", server.address);
    let output = common::command()
        .args(["run", &url, "-r", "v1", "-executor", "simulator"])
        .current_dir(folder.path())
        .env("XDG_CACHE_HOME", folder.path().join("cache"))
        .env_remove("GITEA_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 401"));
}

#[test]
fn runs_remote_documents() {
    let folder = tempfile::tempdir().unwrap();
    let server = common::WebServer::start(&[
        (
            "/tool.cwl",
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs: []\noutputs: []\n",
        ),
        (
            "/workflow.yaml",
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata:\n  name: web\nspec:\n  entrypoint: main\n  templates:\n    - name: main\n      container:\n        image: debian:stable-slim\n        command: [echo, ok]\n",
        ),
    ]);
    for (path, language) in [("/tool.cwl", "cwl"), ("/workflow.yaml", "argo")] {
        let url = format!("{}{path}", server.address);
        run(folder.path(), &url, &[]);
        assert_eq!(
            database(folder.path()).runs(1).unwrap()[0].language,
            language
        );
    }
}
