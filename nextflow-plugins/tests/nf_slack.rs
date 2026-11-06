//! nf-slack against the requests that nf-slack 0.5.1 sent with Nextflow 26.04.6 to fakes of the
//! Slack Web API and a webhook (`req-*.log`); the probes publish below their folder, sleepless.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, Request, recorded};
use serde_json::{Value, json};

const RUN: &str = "nice_miescher";

/// A run of `script` with this `slack` scope, where `{folder}` is the run's folder, and the
/// error that failed it.
fn run(script: &str, scope: &str) -> (tempfile::TempDir, Option<String>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let scope = scope.replace("{folder}", &folder.display().to_string());
    common::copy(&common::fixtures("nf_slack"), folder, &[script]);
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    let config = format!("plugins {{ id 'nf-slack@0.5.1' }}\nslack {{\n{scope}}}\n");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, RUN);
    (directory, execution.error.map(|error| error.to_string()))
}

/// Routes of the Web API like `routes-bot.json`, with the upload address on the fake.
fn bot_routes() -> Value {
    json!([
        {"match": "/api/auth.test", "body": {"ok": true, "team": "Demo team", "user": "demo-bot"}},
        {"match": "/api/chat.", "body": {"ok": true, "channel": "C0DEMO01", "ts": "1793965000.000100"}},
        {"match": "/api/reactions.", "body": {"ok": true}},
        {"match": "/api/files.getUploadURLExternal",
         "body": {"ok": true, "upload_url": "{address}/upload/v1/DEMO", "file_id": "F0DEMO01"}},
        {"match": "/upload/", "body": "OK - 20", "type": "text/plain"},
        {"match": "/api/files.completeUploadExternal", "body": {"ok": true}}
    ])
}

/// A body with the values of this run in place of the recorded ones, and the times masked.
fn normal(body: &Value, folder: &str, command: &str) -> Value {
    match body {
        Value::String(text) => {
            let text = text
                .replace(RUN, "RUN")
                .replace("distraught_raman", "RUN")
                .replace("elegant_hoover", "RUN")
                .replace(folder, "FOLDER")
                .replace(command, "COMMAND");
            let masked = ["*Duration*\n", "*Elapsed*\n"]
                .into_iter()
                .find(|label| text.starts_with(label))
                .map(|label| format!("{label}X"));
            let time = text.contains(", 20") && text.contains(" at ");
            Value::String(masked.unwrap_or(if time { "TIME".into() } else { text }))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| normal(item, folder, command))
                .collect(),
        ),
        Value::Object(entries) => Value::Object(
            (entries.iter())
                .map(|(key, value)| (key.clone(), normal(value, folder, command)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The recorded requests with their work folder and command line made comparable.
fn expected(log: &str, command: &str) -> Vec<(String, Value)> {
    let folder = "/home/beavis/projects/uwe-rs/.claude/plans/plugin-specs/scratch/nf-slack/work";
    (recorded(log).iter())
        .map(|request| {
            let path = request["path"].as_str().unwrap_or_default().to_owned();
            (path, normal(&request["body"], folder, command))
        })
        .collect()
}

fn sent(requests: &[Request], folder: &str) -> Vec<(String, Value)> {
    (requests.iter())
        .map(|request| {
            let body = match request.path.starts_with("/upload/") {
                true => json!({"_text": request.text(), "_bytes": request.body.len()}),
                false => normal(&request.json(), folder, "uwe run main.nf"),
            };
            (request.path.clone(), body)
        })
        .collect()
}

#[test]
fn posts_bot_messages() {
    let fake = Fake::start(bot_routes());
    let scope = format!(
        "  bot {{ token = 'xoxb-not-a-real-token'; channel = 'general'; apiUrl = '{}/api/' }}\n  \
         useThreads = true\n  onProgress {{ enabled = true; interval = '2s' }}\n  onComplete {{ \
         files = ['{{folder}}/results/b.txt'] }}\n",
        fake.address
    );
    let (directory, error) = run("main.nf", &scope);
    assert_eq!(error, None);
    let requests = fake.requests();
    for request in requests
        .iter()
        .filter(|request| request.path.starts_with("/api/"))
    {
        assert_eq!(
            request.header("Authorization"),
            Some("Bearer xoxb-not-a-real-token")
        );
    }
    let work = format!(
        "{}/work",
        std::env::current_dir().expect("a folder").display()
    );
    let mut sent = sent(&requests, &work);
    let mut expected = expected(
        include_str!("fixtures/nf_slack/req-bot.log"),
        "nextflow run main.nf -ansi-log false",
    );
    // Updates between the first and the last one depend on timing; Nextflow's eighth request
    // is its update for a Seqera Platform link, which uwe never has.
    let update = |list: &mut Vec<(String, Value)>| {
        let places: Vec<usize> = (list.iter().enumerate())
            .filter(|(_, (path, _))| path == "/api/chat.update")
            .map(|(place, _)| place)
            .collect();
        for place in places[1..places.len() - 1].iter().rev() {
            list.remove(*place);
        }
    };
    update(&mut sent);
    update(&mut expected);
    let folder = directory.path().display().to_string();
    for (path, body) in &mut sent {
        if path.starts_with("/api/files.getUploadURLExternal") {
            *body = Value::Null;
        }
        *body = Value::String(body.to_string().replace(&folder, "PROJECT")).clone();
    }
    for (_, body) in &mut expected {
        *body = Value::String(body.to_string());
    }
    let paths = |list: &[(String, Value)]| -> Vec<String> {
        list.iter().map(|(path, _)| path.clone()).collect()
    };
    assert_eq!(paths(&sent), paths(&expected));
    for ((path, body), (_, recorded)) in sent.iter().zip(&expected) {
        assert_eq!(body, recorded, "{path}");
    }
}

#[test]
fn posts_webhook_messages() {
    let fake = Fake::start(json!([
        {"match": "/services/T0/B0/good", "body": "ok", "type": "text/plain"}
    ]));
    let scope = format!(
        "  webhook {{ url = '{}/services/T0/B0/good' }}\n  onStart {{ message = 'Run *starting*'; \
         includeCommandLine = false }}\n  onComplete {{\n    message = [text: 'Custom done', color: \
         '#2EB887', includeFields: ['runName', 'duration', 'status', 'tasks', 'commandLine'],\n    \
         customFields: [[title: 'Env', value: 'test', short: true]]]\n  }}\n  reactions {{ enabled \
         = true }}\n",
        fake.address
    );
    let (_directory, error) = run("main.nf", &scope);
    assert_eq!(error, None);
    let requests = fake.requests();
    for request in &requests {
        assert_eq!(request.header("Content-Type"), Some("application/json"));
        assert_eq!(request.header("Authorization"), None);
    }
    let work = format!(
        "{}/work",
        std::env::current_dir().expect("a folder").display()
    );
    let sent = sent(&requests, &work);
    let expected = expected(
        include_str!("fixtures/nf_slack/req-hook.log"),
        "nextflow -C hook.config run main.nf -ansi-log false",
    );
    assert_eq!(sent, expected);
}

#[test]
fn posts_failure_message() {
    let fake = Fake::start(bot_routes());
    let scope = format!(
        "  bot {{ token = 'xoxb-not-a-real-token'; channel = 'general'; apiUrl = '{}/api' }}\n",
        fake.address
    );
    let (_directory, error) = run("fail.nf", &scope);
    assert!(error.is_some());
    let requests = fake.requests();
    let paths: Vec<&str> = requests
        .iter()
        .map(|request| request.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "/api/auth.test",
            "/api/chat.postMessage",
            "/api/chat.postMessage",
            "/api/chat.postMessage",
            "/api/reactions.add",
            "/api/chat.postMessage",
            "/api/reactions.remove",
            "/api/reactions.add",
        ]
    );
    let failure = requests[5].json();
    let texts: Vec<&str> = (failure["blocks"].as_array().into_iter().flatten())
        .flat_map(|block| {
            let fields = block["fields"].as_array().into_iter().flatten();
            let own = block["text"]["text"].as_str();
            own.into_iter()
                .chain(fields.filter_map(|field| field["text"].as_str()))
        })
        .collect();
    assert_eq!(texts[0], "❌ *Pipeline failed*");
    assert_eq!(texts[1], format!("*Run Name*\n{RUN}"));
    assert!(texts[2].starts_with("*Duration*\n"), "{texts:?}");
    assert_eq!(texts[3], "*Status*\n❌ Failed");
    assert_eq!(texts[4], "*Failed Process*\n`SAY`");
    assert_eq!(texts[5], "*Error Message*\n```oops```");
    assert_eq!(texts[6], "*Command Line*\n```uwe run main.nf```");
    assert_eq!(failure["thread_ts"], "1793965000.000100");
    assert_eq!(requests[7].json()["name"], "x");
}
