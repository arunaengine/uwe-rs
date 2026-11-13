//! Posts the chat notification of `imNotification` with the one HTTP client of the process.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::mpsc;
use std::time::Duration;

use nextflow_evaluator::value::Value;

/// How long the request may take.
const LIMIT: Duration = Duration::from_secs(30);

/// Posts JSON text once. The result has the `code` of the answer, or the `error` named as the
/// Java exception that the plugin reports. Neither the address nor the body is logged: the
/// address of a hook holds its credentials.
pub(super) fn post(url: &str, body: &str) -> Value {
    let (url, body) = (url.to_owned(), body.to_owned());
    let (sender, receiver) = mpsc::channel();
    // A thread of its own runs the request, as the caller may already run inside a runtime.
    let started = std::thread::Builder::new()
        .name("nf-core-utils webhook".to_owned())
        .spawn(move || {
            let _ = sender.send(send(&url, body));
        });
    let answer = match started {
        Ok(_) => (receiver.recv_timeout(LIMIT + Duration::from_secs(5)))
            .unwrap_or(Err("SocketTimeoutException")),
        Err(_) => Err("IOException"),
    };
    match answer {
        Ok(code) => Value::map([(Value::string("code"), Value::integer(i32::from(code)))]),
        Err(name) => Value::map([(Value::string("error"), Value::string(name))]),
    }
}

fn send(url: &str, body: String) -> Result<u16, &'static str> {
    let client = storage::http_client().map_err(|_| "IOException")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "IOException")?;
    runtime.block_on(async {
        let request = (client.post(url).timeout(LIMIT))
            .header("Content-Type", "application/json")
            .body(body);
        let response = request.send().await.map_err(|error| {
            if error.is_builder() {
                "MalformedURLException"
            } else if error.is_timeout() {
                "SocketTimeoutException"
            } else if error.is_connect() {
                "ConnectException"
            } else {
                "IOException"
            }
        })?;
        Ok(response.status().as_u16())
    })
}
