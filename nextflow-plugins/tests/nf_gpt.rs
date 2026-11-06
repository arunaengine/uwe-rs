//! nf-gpt against a local fake of an OpenAI-compatible chat completions API; the released
//! plugin fails on Nextflow 26.04.6 before it sends anything, so uwe builds the working calls.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Answers chat completions: data prompts with records, `n: 2` with two choices, `Fail` with a
/// 400; the request bodies it heard.
fn service() -> (u16, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut received = vec![0; length];
            let _ = reader.read_exact(&mut received);
            let body: serde_json::Value = serde_json::from_slice(&received).unwrap_or_default();
            let content = |text: &str| serde_json::json!({ "message": { "role": "assistant", "content": text } });
            let (status, reply) = if body["messages"][0]["content"] == "Fail" {
                ("400 Bad Request", r#"{"error":"nope"}"#.to_owned())
            } else if body.get("response_format").is_some() {
                let records = r#"{"result": [{"name": "Rome", "country": "Italy"}, {"name": "Paris", "country": "France"}]}"#;
                (
                    "200 OK",
                    serde_json::json!({ "choices": [content(records)] }).to_string(),
                )
            } else if body["n"] == 2 {
                (
                    "200 OK",
                    serde_json::json!({ "choices": [content("one"), content("two")] }).to_string(),
                )
            } else {
                (
                    "200 OK",
                    serde_json::json!({ "choices": [content("A joke")] }).to_string(),
                )
            };
            log.lock().expect("the log").push(body);
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (port, heard)
}

#[test]
fn prompts_text_data() {
    let (port, heard) = service();
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-gpt@0.4.0' }}\ngpt {{ endpoint = 'http://127.0.0.1:{port}'; \
         apiKey = 'dummy-key'; model = 'gpt-test'; temperature = 0.2 }}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = "include { gptPromptForText; gptPromptForData; prompt } from 'plugin/nf-gpt'\n\
        workflow {\n  println 'text: ' + gptPromptForText('Tell me a joke')\n  \
        println 'choices: ' + gptPromptForText('Two jokes', numOfChoices: 2, maxTokens: 50, logitBias: ['50256': -100])\n  \
        println 'data: ' + gptPromptForData([schema: [name: 'string', country: 'string']], 'Two cities')\n  \
        channel.fromPrompt([schema: [name: 'string', country: 'string']], 'Cities').view { 'city ' + it.name }\n  \
        channel.of('Europe').prompt([schema: [name: 'string', country: 'string']]) { \"Cities of ${it}\" }.view { 'in ' + it.country }\n}\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "gpt");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    for line in [
        "text: A joke",
        "choices: [one, two]",
        "data: [[name:Rome, country:Italy], [name:Paris, country:France]]",
        "city Rome",
        "city Paris",
        "in France",
    ] {
        assert!(
            printed.lines().any(|printed| printed == line),
            "{line}\n{printed}"
        );
    }
    let heard = heard.lock().expect("the log").clone();
    assert_eq!(
        heard[0],
        serde_json::json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "Tell me a joke" }],
            "n": 1, "temperature": 0.2 })
    );
    assert_eq!(heard[1]["max_tokens"], 50);
    assert_eq!(heard[1]["logit_bias"], serde_json::json!({ "50256": -100 }));
    assert_eq!(
        heard[2]["messages"][0]["content"],
        "You must answer strictly in the following JSON format: {\"result\": [{\"name\": (type: string), \"country\": (type: string)}] }"
    );
    assert_eq!(
        heard[2]["response_format"],
        serde_json::json!({ "type": "json_object" })
    );
    let failing = "include { gptPromptForText } from 'plugin/nf-gpt'\nworkflow {\n  println gptPromptForText('Fail')\n}\n";
    std::fs::write(folder.join("main.nf"), failing).expect("writes the script");
    let (execution, _) = common::run(folder, "gpt");
    let error = execution
        .error
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains(r#"ChatGPT unexpected response: [400] {"error":"nope"}"#),
        "{error}"
    );
    let config = "plugins { id 'nf-gpt@0.4.0' }\ngpt { endpoint = 'http://user:secret@127.0.0.1:9/'; \
                  apiKey = 'dummy-key' }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "gpt");
    let error = execution
        .error
        .map(|error| error.to_string())
        .unwrap_or_default();
    let shown = "Endpoint url must not end with a slash - offending value: http://127.0.0.1:9/";
    assert!(error.contains(shown), "{error}");
}
