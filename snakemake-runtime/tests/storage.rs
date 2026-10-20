//! Checks storage metadata, mapped transfers, default providers, and missing inputs locally.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use snakemake_runtime::{Session, Settings};
use storage::Storage;
use workflow_graph::{Launch, Outcome, OutputFile};

struct Server {
    address: String,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(Mutex::new(HashMap::from([
            ("/in".into(), b"input bytes".to_vec()),
            ("/out".into(), b"old output".to_vec()),
            ("/bucket/in".into(), b"input bytes".to_vec()),
        ])));
        let shared = files.clone();
        let thread = std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let words: Vec<_> = first.split_whitespace().collect();
                if words.get(1) == Some(&"/stop") {
                    break;
                }
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let path = words[1];
                let mut files = shared.lock().unwrap();
                if words[0] == "PUT" {
                    let mut bytes = vec![0; length];
                    reader.read_exact(&mut bytes).unwrap();
                    files.insert(path.into(), bytes);
                }
                let listing = path.contains("list-type=");
                let found = files.get(path);
                let status = if found.is_some() || listing {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let bytes = found.map(Vec::as_slice).unwrap_or(if listing {
                    b"<ListBucketResult></ListBucketResult>"
                } else {
                    b""
                });
                let date =
                    if path.ends_with("/in") || found.is_some_and(|bytes| bytes == b"new output") {
                        "Thu, 01 Jan 1970 00:03:20 GMT"
                    } else {
                        "Thu, 01 Jan 1970 00:01:40 GMT"
                    };
                let tag = bytes.iter().fold(0u64, |tag, byte| {
                    tag.wrapping_mul(31).wrapping_add(u64::from(*byte))
                });
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"{tag:x}\"\r\nLast-Modified: {date}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                if words[0] != "HEAD" {
                    stream.write_all(bytes).unwrap();
                }
            }
        });
        Self {
            address,
            files,
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let mut stream = TcpStream::connect(self.address.trim_start_matches("http://")).unwrap();
        stream.write_all(b"GET /stop HTTP/1.1\r\n\r\n").unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

fn launch(root: &std::path::Path, workflow: &str) -> Launch {
    std::fs::write(root.join("Snakefile"), workflow).unwrap();
    Launch {
        target: root.into(),
        scratch: root.join("scratch"),
        ..Launch::default()
    }
}

#[test]
fn checks_remote_times() {
    let server = Server::start();
    let folder = tempfile::tempdir().unwrap();
    let workflow = format!(
        "storage:\n    provider='http'\nrule make:\n    input: storage.http('{}/in')\n    output: storage.http('{}/out')\n    shell: 'cat {{input}} > {{output}}'\n",
        server.address, server.address
    );
    let launch = launch(folder.path(), &workflow);
    let mut session = Session::new(&launch, &Storage::default()).unwrap();
    let tasks = session.start().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].inputs[0].source, format!("{}/in", server.address));
    assert!(!folder.path().join(&tasks[0].inputs[0].path).exists());
    server
        .files
        .lock()
        .unwrap()
        .insert("/out".into(), b"new output".to_vec());
    let mut session = Session::new(&launch, &Storage::default()).unwrap();
    assert!(session.start().unwrap().is_empty());
    server.files.lock().unwrap().remove("/in");
    let mut session = Session::new(&launch, &Storage::default()).unwrap();
    let error = session.start().unwrap_err();
    assert!(error.message.contains("Missing input"), "{error}");
}

/// Snakemake 9.27 protects only the local copy of a `protected()` output in storage, so the
/// output goes to storage and its delivery protects no local path.
#[test]
fn delivers_protected_objects() {
    let server = Server::start();
    let folder = tempfile::tempdir().unwrap();
    let workflow = format!(
        "storage:\n    provider='http'\nrule make:\n    output: protected(storage.http('{}/new'))\n    shell: 'touch {{output}}'\n",
        server.address
    );
    let launch = launch(folder.path(), &workflow);
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let tasks = driver.start().unwrap();
    let output = folder.path().join("result");
    std::fs::write(&output, b"output bytes").unwrap();
    let file = OutputFile {
        path: tasks[0].outputs[0].clone(),
        location: output.to_string_lossy().into_owned(),
        ..OutputFile::default()
    };
    let outcome = Outcome {
        id: tasks[0].id,
        files: vec![file],
        ..Outcome::default()
    };
    driver.complete(outcome).unwrap();
    let delivery = driver.deliveries().remove(0);
    assert_eq!(delivery.target, format!("{}/new", server.address));
    driver.delivered(&delivery).unwrap();
}

#[test]
fn transfers_s3_objects() {
    let server = Server::start();
    let folder = tempfile::tempdir().unwrap();
    let config = config_file::ConfigFile::parse(&format!("[storage.test]\ntype = 's3'\nprefixes = ['s3://bucket/']\nendpoint = '{}'\nanonymous = true\nregion = 'local'\n", server.address), &folder.path().join("iunctus.toml")).unwrap();
    let storage = Storage::from_config(&config).unwrap();
    let launch = launch(
        folder.path(),
        "storage objects:\n    provider='s3'\nrule make:\n    input: storage.objects('s3://bucket/in')\n    output: storage.objects('s3://bucket/out')\n    shell: 'cat {input} > {output}'\n",
    );
    let mut session = Session::new(&launch, &storage).unwrap();
    let tasks = session.start().unwrap();
    assert_eq!(tasks.len(), 1);
    let input = storage
        .blocking()
        .fetch(&tasks[0].inputs[0].source, &folder.path().join("downloads"))
        .unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), b"input bytes");
    server
        .files
        .lock()
        .unwrap()
        .insert("/bucket/in".into(), b"fresh bytes".to_vec());
    let mut changed = Session::new(&launch, &storage).unwrap();
    let changed = changed.start().unwrap();
    let fresh = storage
        .blocking()
        .fetch(
            &changed[0].inputs[0].source,
            &folder.path().join("downloads"),
        )
        .unwrap();
    assert_ne!(input, fresh);
    assert_eq!(std::fs::read(fresh).unwrap(), b"fresh bytes");
    assert_eq!(std::fs::read(input).unwrap(), b"input bytes");
    let output = folder.path().join("result");
    std::fs::write(&output, b"output bytes").unwrap();
    session
        .complete(Outcome {
            id: tasks[0].id,
            files: vec![OutputFile {
                path: tasks[0].outputs[0].clone(),
                location: output.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .unwrap();
    let deliveries = session.deliveries.lock().unwrap();
    assert_eq!(deliveries[0].target, "s3://bucket/out");
    storage
        .blocking()
        .upload(&output, &deliveries[0].target)
        .unwrap();
    assert_eq!(server.files.lock().unwrap()["/bucket/out"], b"output bytes");
}

#[test]
fn maps_default_storage() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("in"), "input").unwrap();
    let profiles = folder.path().join("profiles/default");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("config.yaml"),
        format!(
            "default-storage-provider: fs\ndefault-storage-prefix: {}\n",
            folder.path().display()
        ),
    )
    .unwrap();
    let launch = launch(
        folder.path(),
        "rule make:\n    input: lambda wildcards: 'in'\n    output: 'out'\n    shell: 'cat {input} > {output}'\n",
    );
    let mut session = Session::new(&launch, &Storage::default()).unwrap();
    let tasks = session.start().unwrap();
    assert_eq!(
        tasks[0].inputs[0].source,
        format!("file://{}/in", folder.path().display())
    );
    assert!(tasks[0].outputs[0].starts_with(".snakemake/storage/fs/"));
    let workflow = "rule make:\n    input: storage(['in'])\n    output: local('out')\n    shell: 'cat {input} > {output}'\n";
    std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
    let mut session = Session::new(&launch, &Storage::default()).unwrap();
    let tasks = session.start().unwrap();
    assert_eq!(
        tasks[0].inputs[0].source,
        format!("file://{}/in", folder.path().display())
    );
    assert_eq!(tasks[0].outputs, ["out"]);
    let override_file = profiles.join("override.yaml");
    std::fs::write(&override_file, "default-storage-prefix: ''\n").unwrap();
    let mut overridden = launch.clone();
    overridden.profiles = vec![
        profiles.join("config.yaml").to_string_lossy().into_owned(),
        override_file.to_string_lossy().into_owned(),
    ];
    let mut session = Session::new(&overridden, &Storage::default()).unwrap();
    assert_eq!(
        session.start().unwrap()[0].inputs[0].path,
        ".snakemake/storage/fs/in"
    );
    for provider in ["sftp", "xrootd", "zenodo", "irods", "unknown"] {
        let launch = launch.clone();
        std::fs::write(
            folder.path().join("Snakefile"),
            format!(
                "rule make:\n    input: storage.{provider}('data')\n    output: local('out')\n"
            ),
        )
        .unwrap();
        let error = snakemake_runtime::driver_with(&launch, Settings::default())
            .unwrap()
            .start()
            .unwrap_err();
        assert!(error.unsupported, "{error}");
    }
}

#[test]
fn stages_unflagged_inputs() {
    let server = Server::start();
    let folder = tempfile::tempdir().unwrap();
    let config = config_file::ConfigFile::parse(&format!("[storage.test]\ntype = 's3'\nprefixes = ['s3://bucket/']\nendpoint = '{}'\nanonymous = true\nregion = 'local'\n", server.address), &folder.path().join("iunctus.toml")).unwrap();
    let storage = Storage::from_config(&config).unwrap();
    let launch = launch(
        folder.path(),
        "rule all:\n    input: 'result'\nrule seed:\n    output: storage.s3('s3://bucket/in')\n    shell: 'false'\nrule make:\n    input: str(rules.seed.output[0])\n    output: 'result'\n    shell: 'cat {input} > {output}'\n",
    );
    let mut session = Session::new(&launch, &storage).unwrap();
    let tasks = session.start().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "make");
    assert_eq!(tasks[0].inputs[0].source, "s3://bucket/in");
}

#[test]
fn retains_storage_times() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["in", "out"] {
        std::fs::write(folder.path().join(name), name).unwrap();
    }
    let launch = launch(
        folder.path(),
        "rule make:\n    input: storage.fs('in')\n    output: storage.fs('out')\n    shell: 'cat {input} > {output}'\n",
    );
    for (input, output, count) in [(200, 100, 1), (200, 300, 0)] {
        for (name, nanos) in [("in", input), ("out", output)] {
            let time = std::time::UNIX_EPOCH + std::time::Duration::new(100, nanos);
            std::fs::File::open(folder.path().join(name))
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(time))
                .unwrap();
        }
        let mut session = Session::new(&launch, &Storage::default()).unwrap();
        assert_eq!(session.start().unwrap().len(), count);
    }
}

/// Snakemake's test_storage registers S3 with `retries=5`, a setting that changes no file;
/// settings with credentials or addresses stay with the storage settings of iunctus, which
/// the refusal names.
#[test]
fn accepts_pacing_settings() {
    let folder = tempfile::tempdir().unwrap();
    for (setting, accepted) in [
        ("retries=5, keep_local=True", true),
        ("endpoint_url='x'", false),
        ("access_key='x'", false),
    ] {
        let workflow = format!(
            "storage:\n    provider='s3', {setting}\nrule make:\n    output: 'out'\n    shell: 'touch {{output}}'\n"
        );
        let launch = launch(folder.path(), &workflow);
        let mut session = Session::new(&launch, &Storage::default()).unwrap();
        match session.start() {
            Ok(tasks) => assert!(accepted && tasks.len() == 1, "{setting}"),
            Err(error) => {
                assert!(!accepted && error.unsupported, "{setting}: {error}");
                // It names the setting of the iunctus config file that replaces it.
                let key = setting.split('=').next().unwrap();
                let replaced = if key == "endpoint_url" {
                    "`endpoint`"
                } else {
                    "`access_key_id`"
                };
                assert!(error.message.contains(replaced), "{error}");
                assert!(error.message.contains("type = \"s3\""), "{error}");
            }
        }
    }
}
