//! Checks a presigned attempt against a fake S3 server: parts of a large output, direct inputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use content_store::Store;
use task_model::{Digest, FORMAT, OutputRule, Placement, TaskSpec};

use super::*;
use crate::capture::RECORDS;
use crate::remote::RemoteStage;
use crate::wrapper::{Folders, wrapper};
use crate::{CONTROL, DirectSource, Finished, HOME, Job, Staging, WORK};

/// What the fake server saw: each request as `METHOD key operation`, and the parts it holds.
#[derive(Default)]
struct Seen {
    calls: Vec<String>,
    parts: BTreeMap<(String, u32), Vec<u8>>,
    /// The headers of the reads of direct inputs.
    tokens: Vec<String>,
    /// What fails: `begin` for the second upload that begins, or `part` for every part.
    fail: Option<&'static str>,
}

/// The least size of a part but the last that the fake takes, as S3 takes 5 MiB.
const LEAST: usize = 10;

/// An S3 server whose bucket `bucket` is the folder `root`, which also serves `data/ref.txt`.
fn fake_s3(root: PathBuf) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let (mut length, mut token) = (0, String::new());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                match line.split_once(':') {
                    _ if line.trim().is_empty() => break,
                    Some((name, value)) if name.eq_ignore_ascii_case("content-length") => {
                        length = value.trim().parse().unwrap();
                    }
                    Some((name, value)) if name.eq_ignore_ascii_case("x-token") => {
                        token = value.trim().to_owned();
                    }
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut words = first.split_whitespace();
            let method = words.next().unwrap().to_owned();
            let target = words.next().unwrap().to_owned();
            let (path, query) = target.split_once('?').unwrap_or((&target, ""));
            let key = path.trim_start_matches("/bucket/").to_owned();
            let value = |name: &str| {
                (query.split('&'))
                    .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                    .map(str::to_owned)
            };
            let mut seen = kept.lock().unwrap();
            let (status, headers, answer) = match method.as_str() {
                "GET" if path == "/data/ref.txt" => {
                    seen.tokens.push(token);
                    ("200 OK", String::new(), "reference\n".to_owned())
                }
                "POST" if query.split('&').any(|pair| pair.starts_with("uploads")) => {
                    let id = format!("upload-{}", seen.calls.len());
                    seen.calls.push(format!("POST {key} begin"));
                    let begun = (seen.calls.iter()).filter(|call| call.ends_with("begin"));
                    match seen.fail == Some("begin") && begun.count() > 1 {
                        true => ("503 Slow Down", String::new(), String::new()),
                        false => {
                            let text = format!("<Result><UploadId>{id}</UploadId></Result>");
                            ("200 OK", String::new(), text)
                        }
                    }
                }
                "PUT" if seen.fail == Some("part") => {
                    seen.calls.push(format!("PUT {key} failed"));
                    ("500 Internal Server Error", String::new(), String::new())
                }
                "PUT" => {
                    let number: u32 = value("partNumber").unwrap().parse().unwrap();
                    seen.calls.push(format!("PUT {key} part {number}"));
                    seen.parts.insert((key.clone(), number), body);
                    (
                        "200 OK",
                        format!("ETag: \"tag-{number}\"\r\n"),
                        String::new(),
                    )
                }
                "POST" => {
                    let text = String::from_utf8(body).unwrap();
                    seen.calls.push(format!("POST {key} complete {text}"));
                    let mut joined = Vec::new();
                    let numbers: Vec<u32> = (text.split("<PartNumber>").skip(1))
                        .map(|part| part.split('<').next().unwrap().parse().unwrap())
                        .collect();
                    for (index, number) in numbers.iter().enumerate() {
                        let part = seen.parts.remove(&(key.clone(), *number)).unwrap();
                        assert!(
                            index + 1 == numbers.len() || part.len() >= LEAST,
                            "S3 refuses a part of {} bytes but the last",
                            part.len()
                        );
                        joined.extend(part);
                    }
                    let file = root.join(&key);
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(file, joined).unwrap();
                    (
                        "200 OK",
                        String::new(),
                        "<CompleteMultipartUploadResult/>".into(),
                    )
                }
                "DELETE" => {
                    seen.calls.push(format!("DELETE {key} abort"));
                    ("204 No Content", String::new(), String::new())
                }
                _ => ("400 Bad Request", String::new(), String::new()),
            };
            drop(seen);
            let reply = format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n\
                 {answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (address, seen)
}

fn spec() -> TaskSpec {
    TaskSpec {
        output_manifest: None,
        format: FORMAT,
        arguments: vec![
            "sh".into(),
            "-c".into(),
            "printf 'abcdefghijklmnopqrst' > big.txt && cp ref.txt small.txt".into(),
        ],
        namespace: String::new(),
        image: None,
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: ["big.txt", "small.txt"]
            .map(|pattern| OutputRule {
                pattern: pattern.into(),
            })
            .to_vec(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
        run_as: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_large_parts() {
    let objects = tempfile::tempdir().unwrap();
    let node = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (address, seen) = fake_s3(objects.path().to_owned());
    let keys = storage::aws::Keys {
        id: "id".into(),
        secret: "secret".into(),
        token: None,
    };
    let multipart = Multipart {
        files: 2,
        parts: 3,
        threshold: 12,
        least_part: LEAST as u64,
        ..Multipart::new(&address, "us-east-1", "s3://bucket", keys).unwrap()
    };
    let stage = RemoteStage {
        objects: Store::local(objects.path()).unwrap(),
        expiry: Duration::from_secs(600),
        max_outputs: 8,
        credentials: false,
        cleanup: false,
        multipart: Some(multipart),
    };
    let spec = spec();
    let reference = b"reference\n";
    let staging = Staging {
        direct: vec![DirectSource {
            path: "ref.txt".into(),
            source: format!("{address}/data/ref.txt"),
            digest: Digest::sha256(reference),
            size: reference.len() as u64,
            executable: false,
            reference: storage::DirectRef {
                url: format!("{address}/data/ref.txt"),
                headers: vec![("x-token".into(), "secret-token".into())],
                ..storage::DirectRef::default()
            },
            endpoints: Vec::new(),
        }],
        ..Staging::default()
    };
    let placement = Placement::default();
    let job = Job {
        name: "large-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let root = node.path().join("attempt");
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let (work, control) = (text(&root.join(WORK)), text(&root.join(CONTROL)));
    let (home, outbox) = (text(&root.join(HOME)), text(&root.join("outbox")));
    let folders = Folders {
        work: &work,
        control: &control,
        home: &home,
        outbox: &outbox,
        owner: "",
    };
    let script = wrapper(&spec, &folders);
    let script = crate::capture::capture(&script, &text(&root.join("transfer"))).unwrap();
    let extra = [("control/wrapper.sh", script)];
    let planned = stage
        .plan(job, &root, &extra, &[], RECORDS, false)
        .await
        .unwrap();
    let (folder, location) = (text(&root), planned.location.clone());
    tokio::task::spawn_blocking(move || {
        crate::helper::main(&["in".into(), folder.clone(), location]).unwrap();
        let run = std::process::Command::new("sh")
            .arg(format!("{folder}/control/wrapper.sh"))
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        crate::helper::main(&["out".into(), folder]).unwrap();
    })
    .await
    .unwrap();
    let host = scratch.path().join("attempt");
    stage
        .collect(&planned.prefix, &host, RECORDS)
        .await
        .unwrap();
    let finished = Finished::from_control(host).unwrap();
    assert_eq!(finished.exit_code, Some(0));
    let read = |path: &str| std::fs::read_to_string(finished.local_path(path)).unwrap();
    assert_eq!(read("big.txt"), "abcdefghijklmnopqrst");
    assert_eq!(read("small.txt"), "reference\n");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.tokens,
        ["secret-token"],
        "the helper read the direct input once"
    );
    let slot = |number: u32| format!("{}/large/{number}", planned.prefix);
    // 20 bytes in at most 3 parts of at least 10 bytes: two of 10, then the list of their tags.
    let complete = format!(
        "POST {} complete <CompleteMultipartUpload>{}</CompleteMultipartUpload>",
        slot(1),
        (1..=2)
            .map(|number| {
                format!(
                    "<Part><PartNumber>{number}</PartNumber><ETag>\"tag-{number}\"</ETag></Part>"
                )
            })
            .collect::<String>()
    );
    let expected = [
        format!("POST {} begin", slot(1)),
        format!("POST {} begin", slot(2)),
        format!("PUT {} part 1", slot(1)),
        format!("PUT {} part 2", slot(1)),
        complete,
        format!("DELETE {} abort", slot(2)),
    ];
    assert_eq!(seen.calls, expected);
}

fn multipart(address: &str) -> Multipart {
    let keys = storage::aws::Keys {
        id: "id".into(),
        secret: "secret".into(),
        token: None,
    };
    Multipart {
        files: 2,
        parts: 3,
        threshold: 12,
        least_part: LEAST as u64,
        ..Multipart::new(address, "us-east-1", "s3://bucket", keys).unwrap()
    }
}

/// An upload that cannot begin aborts those that began before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_begun_uploads() {
    let objects = tempfile::tempdir().unwrap();
    let (address, seen) = fake_s3(objects.path().to_owned());
    seen.lock().unwrap().fail = Some("begin");
    let begun = multipart(&address)
        .begin("attempt", Duration::from_secs(600))
        .await;
    assert!(begun.is_err());
    let calls = seen.lock().unwrap().calls.clone();
    let expected = [
        "POST attempt/large/1 begin",
        "POST attempt/large/2 begin",
        "DELETE attempt/large/1 abort",
    ];
    assert_eq!(calls, expected);
}

/// A part that does not go up aborts its upload and the unused ones, before the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_failed_parts() {
    let objects = tempfile::tempdir().unwrap();
    let (address, seen) = fake_s3(objects.path().to_owned());
    let large = multipart(&address)
        .begin("attempt", Duration::from_secs(600))
        .await;
    seen.lock().unwrap().fail = Some("part");
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(folder.path().join("transfer")).unwrap();
    std::fs::write(folder.path().join("transfer/1"), "abcdefghijklmnopqrst").unwrap();
    let plan = crate::helper::Plan {
        files: vec![crate::helper::Request {
            method: "PUT".into(),
            url: format!("{address}/bucket/attempt/files/1"),
            headers: Vec::new(),
        }],
        large: large.unwrap(),
        threshold: Some(12),
        least_part: Some(LEAST as u64),
        ..crate::helper::Plan::default()
    };
    let path = folder.path().to_owned();
    let out = tokio::task::spawn_blocking(move || crate::helper::stage_out(&path, None, &plan));
    assert!(out.await.unwrap().is_err());
    let calls = seen.lock().unwrap().calls.clone();
    assert!(
        calls.contains(&"DELETE attempt/large/1 abort".to_owned()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"DELETE attempt/large/2 abort".to_owned()),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("complete")),
        "{calls:?}"
    );
}

#[test]
fn refuses_unbucketed_roots() {
    let keys = storage::aws::Keys {
        id: "id".into(),
        secret: "secret".into(),
        token: None,
    };
    let endpoint = "https://s3.example.org/";
    assert!(Multipart::new(endpoint, "us-east-1", "gs://bucket", keys.clone()).is_err());
    assert!(Multipart::new(endpoint, "us-east-1", "s3://", keys.clone()).is_err());
    let made = Multipart::new(endpoint, "us-east-1", "s3://bucket/a/b/", keys).unwrap();
    assert_eq!(
        (made.bucket.as_str(), made.root.as_str()),
        ("bucket", "a/b")
    );
    assert_eq!(made.endpoint, "https://s3.example.org");
    assert!(made.threshold < crate::helper::MOST_BYTES);
}
