//! Checks Snakemake Conda digest submission through TES without package downloads.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conda_environment::{BuiltImage, Environment, ImagePreparation};
use content_store::Store;
use serde_json::{Value, json};

mod common;

fn request(stream: &mut impl Read) -> (String, String) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (first, String::from_utf8(body).unwrap())
}

struct Server {
    address: String,
    task: Arc<Mutex<Option<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = task.clone();
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let (first, body) = request(&mut stream);
                let answer = if first.starts_with("HEAD /v2/tool/manifests/") {
                    String::new()
                } else if first.starts_with("GET /v2/tool/manifests/") {
                    json!({"schemaVersion":2,"config":{"digest":format!("sha256:{}", "b".repeat(64))}}).to_string()
                } else if first.starts_with("GET /v2/tool/blobs/") {
                    json!({"os":"linux","architecture":"amd64","config":{}}).to_string()
                } else if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                    let task: Value = serde_json::from_str(&body).unwrap();
                    for output in task["outputs"].as_array().unwrap() {
                        let folder = Path::new(
                            output["url"]
                                .as_str()
                                .unwrap()
                                .strip_prefix("file://")
                                .unwrap(),
                        );
                        std::fs::create_dir_all(folder).unwrap();
                        if output["path"] == "/iunctus/control" {
                            std::fs::write(folder.join("exit"), "0\n").unwrap();
                            std::fs::write(folder.join("manifest"), "file out.txt\n").unwrap();
                        } else {
                            std::fs::write(folder.join("out.txt"), "prepared\n").unwrap();
                        }
                    }
                    *recorded.lock().unwrap() = Some(task);
                    json!({"id":"prepared"}).to_string()
                } else if first.contains("/tasks/prepared") {
                    json!({"id":"prepared","state":"COMPLETE"}).to_string()
                } else {
                    json!({"tasks":[]}).to_string()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nDocker-Content-Digest: sha256:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", "c".repeat(64), answer.len()).unwrap();
            }
        });
        Self {
            address,
            task,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn engine(root: &Path, replies: Vec<(String, String)>) -> std::thread::JoinHandle<()> {
    let listener = std::os::unix::net::UnixListener::bind(root.join("engine.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        for (path, body) in [
            ("/version".into(), "{}".into()),
            ("/info".into(), "{}".into()),
        ]
        .into_iter()
        .chain(replies)
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing engine request"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (first, _) = request(&mut stream);
            assert!(first.contains(&path));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    })
}

#[tokio::test]
async fn submits_conda_digest() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let server = Server::start();
    let registry = format!("{}/tool", server.address);
    let reference = format!("{registry}@sha256:{}", "c".repeat(64));
    let base = format!("mamba@sha256:{}", "a".repeat(64));
    let repodata = root.join("repodata");
    for subdir in ["linux-64", "noarch"] {
        let path = repodata.join("conda-forge").join(subdir);
        std::fs::create_dir_all(&path).unwrap();
        let metadata = if subdir == "linux-64" {
            json!({"packages":{"tiny-1-0.tar.bz2":{"name":"tiny","version":"1","build":"0","build_number":0,"subdir":"linux-64","sha256":"d".repeat(64)}}})
        } else {
            json!({})
        };
        std::fs::write(path.join("repodata.json"), metadata.to_string()).unwrap();
    }
    let source = "channels: [conda-forge]\ndependencies: [tiny=1]\n";
    std::fs::write(root.join("env.yaml"), source).unwrap();
    let environment = Environment::parse(source).unwrap();
    let lock =
        conda_environment::lock_with_cache(&environment, &repodata, "linux-64", &[], &repodata)
            .unwrap();
    let preparation = ImagePreparation::new(&lock, &base, "linux/amd64", &[]).unwrap();
    let receipt = preparation
        .completed(BuiltImage {
            preparation: preparation.digest().into(),
            base: base.clone(),
            recipe_digest: preparation.recipe_digest(),
            platform: "linux/amd64".into(),
            image_id: format!("sha256:{}", "b".repeat(64)),
            manifest_digest: Some(format!("sha256:{}", "c".repeat(64))),
            archive_digest: None,
            reference: Some(reference.clone()),
        })
        .unwrap();
    let store = Store::local(&root.join("store")).unwrap();
    store
        .put_entry(
            "conda-registry",
            &preparation.digest().parse().unwrap(),
            serde_json::to_vec(&receipt).unwrap().into(),
        )
        .await
        .unwrap();
    std::fs::write(
        root.join("target.json"),
        json!({"base":base,"platform":"linux/amd64","virtual_packages":[]}).to_string(),
    )
    .unwrap();
    let settings = format!(
        "[cache]\nfolder = '{0}/cache'\n[store]\nlocation = '{0}/store'\n[executor.local]\nengine = '{0}/engine.sock'\n[conda]\nbase = '{base}'\nplatform = 'linux/amd64'\ninstaller = 'micromamba'\noffline = true\ncapabilities = '{0}/target.json'\nrepodata = '{0}/repodata'\npackages = '{0}/missing-packages'\n",
        root.display()
    );
    let config = root.join("config.toml");
    std::fs::write(&config, format!("{settings}registry = '{registry}'\n")).unwrap();
    std::fs::write(root.join("Snakefile"), "rule all:\n    output: 'out.txt'\n    conda: 'env.yaml'\n    shell: 'printf prepared > {output}'\n").unwrap();
    let engine = engine(root, vec![]);
    let run = || {
        common::command()
            .args(["run", "Snakefile", "-executor", "tes", "-tes-server"])
            .arg(format!("http://{}", server.address))
            .arg("-iunctus-config")
            .arg(&config)
            .args(["-no-cache", "-forceall"])
            .env("UWE_LOG", "debug")
            .current_dir(root)
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    engine.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("out.txt")).unwrap(),
        "prepared\n"
    );
    assert_eq!(
        server.task.lock().unwrap().as_ref().unwrap()["executors"][0]["image"],
        reference
    );
    std::fs::remove_file(root.join("out.txt")).unwrap();
    *server.task.lock().unwrap() = None;
    std::fs::write(&config, &settings).unwrap();
    let output = run();
    assert_eq!(
        output.status.code(),
        Some(33),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("[conda].registry"));
    assert!(server.task.lock().unwrap().is_none());
    let key = preparation.digest().parse().unwrap();
    let observed = store
        .observe_entry("conda-registry", &key)
        .await
        .unwrap()
        .unwrap();
    store
        .invalidate_entry("conda-registry", &key, &observed)
        .await
        .unwrap();
    store
        .put_entry(
            conda_environment::IMAGE_CACHE_NAMESPACE,
            &key,
            serde_json::to_vec(&receipt).unwrap().into(),
        )
        .await
        .unwrap();
    std::fs::remove_file(root.join("engine.sock")).unwrap();
    let image = json!({"Id":receipt.image_id(),"Os":"linux","Architecture":"amd64","Config":{"Labels":{
        "org.iunctus.conda.preparation":preparation.digest(),"org.iunctus.conda.base":preparation.base(),"org.iunctus.conda.recipe":preparation.recipe_digest()
    }}}).to_string();
    let pushing = crate::engine(
        root,
        vec![
            ("/json".into(), image.clone()),
            ("/json".into(), image),
            ("/tag?".into(), String::new()),
            (
                "/push?".into(),
                "{\"errorDetail\":{\"message\":\"private-value\"}}\n".into(),
            ),
        ],
    );
    std::fs::write(&config, format!("{settings}registry = '{registry}'\n")).unwrap();
    let output = run();
    pushing.join().unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(33));
    assert!(error.contains("Conda image cache hit"));
    assert!(!error.contains("private-value"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-value"));
    assert!(server.task.lock().unwrap().is_none());
}
