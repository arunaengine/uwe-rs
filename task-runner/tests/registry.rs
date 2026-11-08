//! Checks that task attempts sign in to registries with the registry settings of their run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The futures of attempts nest deeply, deeper than the default limit lets the compiler check.
#![recursion_limit = "256"]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

use content_store::Store;
use executor::credentials::Registry;
use executor::{Backend, ExecutorError, Finished, Job};
use task_runner::{Runner, RunnerExecutor, Settings};
use tokio_util::sync::CancellationToken;
use workflow_graph::{Attempt, Executor, Task, TaskId};

/// A backend whose pins and runs ask a registry that needs the sign-in of the run.
struct Asking {
    registry: String,
    /// Whether the registry answered each run.
    answered: Mutex<Vec<bool>>,
}

impl Backend for Asking {
    async fn pin(&self, _: &str) -> Result<Option<String>, ExecutorError> {
        executor::registry::pin(&format!("{}/tool:1", self.registry)).await
    }

    async fn run(&self, _: Job<'_>, _: &CancellationToken) -> Result<Finished, ExecutorError> {
        let answer = executor::registry::pin(&format!("{}/tool:1", self.registry)).await;
        self.answered.lock().unwrap().push(answer.is_ok());
        Err(ExecutorError::Unsupported("not run by this test".into()))
    }
}

/// A registry on a local port that answers only requests that sign in as `user`.
fn registry() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut signed = false;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                signed |= line.trim_end() == "authorization: Basic dXNlcjpzZWNyZXQ=";
                line.clear();
            }
            let digest = "a".repeat(64);
            let answer = match signed {
                true => format!("200 OK\r\ndocker-content-digest: sha256:{digest}"),
                false => "401 Unauthorized\r\nwww-authenticate: Basic realm=\"registry\"".into(),
            };
            let answer =
                format!("HTTP/1.1 {answer}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    address
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attempts_use_registry() {
    let root = tempfile::tempdir().unwrap();
    let address = registry();
    let text = format!("[registry.\"{address}\"]\nusername = \"user\"\npassword = \"secret\"");
    let config = config_file::ConfigFile::parse(&text, Path::new("uwe.toml")).unwrap();
    let backend = Asking {
        registry: address,
        answered: Mutex::default(),
    };
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
        digests: None,
    };
    let runner = Runner::new(Store::memory().unwrap(), backend, "signed", settings);
    let mut executor = RunnerExecutor::new(runner, None);
    // The run alone knows the sign-in: no request entered it in this process.
    executor.use_registry(Registry::from_config(&config).unwrap().without_files());
    let attempt = executor.run(Attempt {
        task: std::sync::Arc::new(Task {
            id: TaskId(1),
            image: Some("tool".into()),
            command: vec!["true".into()],
            ..Task::default()
        }),
        number: 1,
        cancel: CancellationToken::new(),
    });
    let result = tokio::spawn(attempt).await.unwrap();
    let error = result
        .expect_err("the backend does not run tasks")
        .to_string();
    assert!(error.contains("not run by this test"), "{error}");
    let answered = executor.runner().backend().answered.lock().unwrap().clone();
    assert_eq!(
        answered,
        [true],
        "the pin and the run sign in to the registry"
    );
}
