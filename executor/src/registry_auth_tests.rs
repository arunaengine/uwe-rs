//! Checks registry authentication exchanges and secret redaction over loopback HTTP.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

use super::*;

enum Flow {
    Basic,
    Identity,
    Direct,
    Bearer,
    Helper,
    Refused,
}

fn registry(flow: Flow) -> (Reference, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let realm = format!("http://{host}/token?audience=fake");
    let challenge = match flow {
        Flow::Direct => "Basic realm=\"registry\"".to_owned(),
        _ => {
            format!("Bearer realm=\"{realm}\",service=\"registry\",scope=\"repository:tool:pull\"")
        }
    };
    let requests = match flow {
        Flow::Basic | Flow::Identity => 3,
        _ => 2,
    };
    let thread = std::thread::spawn(move || {
        for index in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let first = line.clone();
            let mut auth = None;
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("authorization") {
                        auth = Some(value.trim().to_owned());
                    }
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let (status, headers, answer) = if index == 0 {
                assert!(first.starts_with("HEAD /v2/tool/manifests/1 "));
                assert_eq!(auth, None);
                (
                    "401 Unauthorized",
                    format!("WWW-Authenticate: {challenge}\r\n"),
                    "",
                )
            } else if first.contains("/token?") {
                assert!(first.contains("audience=fake"));
                match flow {
                    Flow::Identity => {
                        assert!(first.starts_with("POST "));
                        assert_eq!(auth, None);
                        assert!(body.contains("grant_type=refresh_token"));
                        assert!(
                            body.contains("refresh_token=refresh%26value%3D%20"),
                            "{body}"
                        );
                        assert!(body.contains("client_id=iunctus"));
                        assert!(body.contains("service=registry"));
                        assert!(body.contains("scope=repository:tool:pull"));
                    }
                    Flow::Basic | Flow::Refused => {
                        assert!(first.starts_with("GET "));
                        assert_eq!(
                            auth.as_deref(),
                            Some(basic("user", "private-value").to_str().unwrap())
                        );
                        assert!(first.contains("service=registry"));
                        assert!(first.contains("scope=repository%3Atool%3Apull"));
                    }
                    Flow::Helper => assert_eq!(auth, None),
                    _ => panic!("unexpected exchange"),
                }
                if matches!(flow, Flow::Refused | Flow::Helper) {
                    (
                        "401 Unauthorized",
                        String::new(),
                        "private-value refresh&value= access-value",
                    )
                } else {
                    (
                        "200 OK",
                        String::new(),
                        r#"{"access_token":"access-value"}"#,
                    )
                }
            } else {
                let expected = if matches!(flow, Flow::Direct) {
                    basic("user", "private-value")
                } else {
                    bearer("access-value").unwrap()
                };
                assert_eq!(auth.as_deref(), Some(expected.to_str().unwrap()));
                ("200 OK", "Docker-Content-Digest: sha256:abc\r\n".into(), "")
            };
            let length = if first.starts_with("HEAD ") {
                0
            } else {
                answer.len()
            };
            write!(stream, "HTTP/1.1 {status}\r\n{headers}Content-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
            if length > 0 {
                stream.write_all(answer.as_bytes()).unwrap();
            }
        }
    });
    (Reference::parse(&format!("{host}/tool:1")), thread)
}

async fn resolve(flow: Flow, found: Found) -> Result<String, ExecutorError> {
    let (parsed, thread) = registry(flow);
    let result = ask_with(
        &parsed,
        Method::HEAD,
        &manifest_url(&parsed),
        MANIFESTS,
        Some(found),
    )
    .await;
    thread.join().unwrap();
    result.map(|response| {
        response.headers()["docker-content-digest"]
            .to_str()
            .unwrap()
            .to_owned()
    })
}

fn password() -> Found {
    Found {
        credential: Some(Credential::Basic("user".into(), "private-value".into())),
        helper: false,
    }
}

#[tokio::test]
async fn basic_token_flow() {
    assert_eq!(
        resolve(Flow::Basic, password()).await.unwrap(),
        "sha256:abc"
    );
}

#[tokio::test]
async fn identity_token_flow() {
    let found = Found {
        credential: Some(Credential::Identity("refresh&value= ".into())),
        helper: false,
    };
    assert_eq!(resolve(Flow::Identity, found).await.unwrap(), "sha256:abc");
}

#[tokio::test]
async fn direct_basic_flow() {
    assert_eq!(
        resolve(Flow::Direct, password()).await.unwrap(),
        "sha256:abc"
    );
}

#[tokio::test]
async fn configured_bearer_flow() {
    let found = Found {
        credential: Some(Credential::Bearer("access-value".into())),
        helper: false,
    };
    assert_eq!(resolve(Flow::Bearer, found).await.unwrap(), "sha256:abc");
}

#[tokio::test]
async fn explains_helper_credentials() {
    let found = Found {
        credential: None,
        helper: true,
    };
    let error = resolve(Flow::Helper, found).await.unwrap_err().to_string();
    assert!(
        error.contains("credential helper") && error.contains("cannot run external helpers"),
        "{error}"
    );
    assert!(!error.contains("private-value"));
}

#[tokio::test]
async fn redacts_token_failure() {
    let error = resolve(Flow::Refused, password())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("401"), "{error}");
    for secret in ["private-value", "refresh&value=", "access-value"] {
        assert!(!error.contains(secret), "{error}");
    }
    assert!(basic("user", "private-value").is_sensitive());
    assert!(bearer("access-value").unwrap().is_sensitive());
    assert!(
        !bearer("private-value\n")
            .unwrap_err()
            .contains("private-value")
    );
}
