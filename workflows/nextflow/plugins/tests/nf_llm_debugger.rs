//! nf-llm-debugger against the requests that nf-llm-debugger 1.0.14 sent with Nextflow 26.04.6
//! to a local fake server (`req-*.log`), for the failing probe pipeline `main.nf`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, Request, keys, recorded};
use serde_json::{Value, json};

/// Answers like the recorded routes: OpenAI, proxy, Ollama, Gemini and Claude forms.
fn routes() -> Value {
    json!([
        {"match": "/chat/completions", "body": {"choices": [{"index": 0, "message":
            {"role": "assistant", "content": "The file is missing; stage it as an input."}}]}},
        {"match": "/debug/nextflow", "body": {"result": "Proxy diagnosis.", "model": "proxy-model",
            "note": "demo note"}},
        {"match": "/api/chat", "body": {"message": {"role": "assistant", "content": "Ollama."}}},
        {"match": ":generateContent", "body": {"candidates": [{"content": {"parts":
            [{"text": "Gemini "}, {"text": "diagnosis."}]}}]}},
        {"match": "/v1/messages", "body": {"content": [{"type": "text", "text": "Claude."}]}},
        {"match": "/", "body": {}}
    ])
}

/// A failed run of the probe pipeline with these params, and the requests it sent.
fn run(fake: &Fake, params: &str, name: &str) -> Vec<Request> {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(
        &common::fixtures("nf_llm_debugger"),
        folder,
        &["main.nf", "docs.txt"],
    );
    let params = params
        .replace("{address}", &fake.address)
        .replace("{folder}", &folder.to_string_lossy());
    let config = format!(
        "plugins {{ id 'nf-llm-debugger@1.0.14' }}\nparams {{\n  llm_api_key = \
         'not-a-real-key'\n{params}}}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, name);
    assert!(execution.error.is_some(), "the probe fails");
    fake.requests()
}

fn body(log: &str) -> Value {
    recorded(log)[0]["body"].clone()
}

#[test]
fn off_by_default() {
    let fake = Fake::start(routes());
    let requests = run(&fake, "  llm_endpoint = '{address}/v1'\n", "quiet_run");
    assert!(requests.is_empty());
}

#[test]
fn asks_compatible_model() {
    let fake = Fake::start(routes());
    let params = "  llm_enabled = true\n  llm_endpoint = '{address}/v1'\n  llm_model = \
                  'demo-model'\n  llm_docs = '{folder}/docs.txt'\n";
    let requests = run(&fake, params, "local_run");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert_eq!(
        request.header("Authorization"),
        Some("Bearer not-a-real-key")
    );
    let sent = request.json();
    let expected = body(include_str!("fixtures/nf_llm_debugger/req-local.log"));
    assert_eq!(keys(&sent), keys(&expected));
    assert_eq!(
        (sent["model"].clone(), sent["temperature"].clone()),
        (json!("demo-model"), json!(0.2))
    );
    let system = sent["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(
        system.ends_with("as background:\n# Pipeline notes\nInputs live in data/."),
        "{system}"
    );
    let user = sent["messages"][1]["content"].as_str().unwrap_or_default();
    assert!(
        user.starts_with("Task failProcess [1] failed.\nWork Directory: null\n\nError report:\n"),
        "{user}"
    );
    assert!(user.ends_with("\n\nAnswer in at most two sentences, without lists or headings."));
}

#[test]
fn asks_proxy_ollama() {
    let fake = Fake::start(routes());
    let params = "  llm_enabled = true\n  llm_endpoint = '{address}/llm-proxy'\n  llm_answer = \
                  'extensive'\n  llm_docs = '{folder}/docs.txt'\n";
    let requests = run(&fake, params, "proxy_run");
    assert_eq!(requests[0].path, "/llm-proxy/debug/nextflow");
    let expected = body(include_str!("fixtures/nf_llm_debugger/req-proxy.log"));
    let sent = requests[0].json();
    assert_eq!(keys(&sent), keys(&expected));
    let log = sent["log"].as_str().unwrap_or_default();
    assert!(log.contains("Task failProcess [1] failed.") && log.contains("Inputs live in data/."));

    let fake = Fake::start(routes());
    let params = "  llm_enabled = true\n  llm_endpoint = '{address}/ollama'\n  llm_model = \
                  'llama3'\n  llm_answer = 'detailed'\n";
    let requests = run(&fake, params, "ollama_run");
    assert_eq!(requests[0].path, "/ollama/api/chat");
    let expected = body(include_str!("fixtures/nf_llm_debugger/req-ollama.log"));
    let sent = requests[0].json();
    assert_eq!(keys(&sent), keys(&expected));
    assert_eq!(
        (sent["stream"].clone(), sent["options"].clone()),
        (json!(false), json!({"temperature": 0.2}))
    );
    assert_eq!(requests[0].header("Authorization"), None);
}

#[test]
fn asks_gemini_claude() {
    let fake = Fake::start(routes());
    let params = "  llm_enabled = true\n  llm_endpoint = '{address}/v1beta'\n  llm_model = \
                  'gemini-2.0-flash'\n";
    let requests = run(&fake, params, "gemini_run");
    assert_eq!(
        requests[0].path,
        "/v1beta/models/gemini-2.0-flash:generateContent"
    );
    assert_eq!(requests[0].header("x-goog-api-key"), Some("not-a-real-key"));
    let sent = requests[0].json();
    assert_eq!(sent["generationConfig"], json!({"temperature": 0.2}));
    assert_eq!(sent["contents"][0]["role"], "user");
    assert!(sent["systemInstruction"]["parts"][0]["text"].is_string());

    let fake = Fake::start(routes());
    let params = "  llm_enabled = true\n  llm_endpoint = '{address}'\n  llm_model = \
                  'claude-sonnet-5'\n";
    let requests = run(&fake, params, "claude_run");
    assert_eq!(requests[0].path, "/v1/messages");
    assert_eq!(requests[0].header("x-api-key"), Some("not-a-real-key"));
    assert_eq!(requests[0].header("anthropic-version"), Some("2023-06-01"));
    let sent = requests[0].json();
    assert_eq!(sent["model"], "claude-sonnet-5");
    assert_eq!(sent["messages"][0]["role"], "user");
    assert!(sent["system"].is_string() && sent["max_tokens"].is_number());
}
