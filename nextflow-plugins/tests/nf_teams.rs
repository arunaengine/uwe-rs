//! nf-teams against the cards that nf-teams 0.2.2 posted with Nextflow 26.04.6 to a local
//! fake webhook (`req-*.log`); the main probe is the one of the nf-tower fixtures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, recorded};
use serde_json::{Value, json};

const RUN: &str = "sharp_lalande";

/// A run of `script` posting to `fake` with these `teams` settings, and its error.
fn run(fixture: &str, script: &str, fake: &Fake, events: &str) -> Option<String> {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures(fixture), folder, &[script]);
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    let config = format!(
        "plugins {{ id 'nf-teams@0.2.2' }}\nmanifest {{ name = 'demo/probe'; version = '1.0.0' \
         }}\nteams {{\n  enabled = true\n  webHook {{ url = '{}/webhook/abc' }}\n{events}}}\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, RUN);
    execution.error.map(|error| error.to_string())
}

const ALL: &str = "  onStart { enabled = true }\n  onSuccess { enabled = true }\n  onError { \
                   enabled = true }\n  onComplete { enabled = true }\n";

/// A card with the values of the run, times and the launch command masked.
fn normal(card: &Value) -> Value {
    match card {
        Value::String(text) => {
            let text = (text.replace(RUN, "RUN"))
                .replace("spontaneous_booth", "RUN")
                .replace("nextflow run main.nf -ansi-log false", "COMMAND")
                .replace("nextflow run fail.nf -ansi-log false", "COMMAND")
                .replace("uwe run main.nf", "COMMAND");
            let prefixes = ["Started on ", "Completed on ", "Failed on "];
            let prefix = prefixes.into_iter().find(|prefix| text.starts_with(prefix));
            let time = text.len() > 20 && text.as_bytes()[4] == b'-' && text.contains('T');
            Value::String(match (prefix, time) {
                (Some(prefix), _) => format!("{prefix}X"),
                (None, true) => "TIME".into(),
                (None, false) => text,
            })
        }
        Value::Array(items) => Value::Array(items.iter().map(normal).collect()),
        Value::Object(entries) => {
            let title = entries.get("title").and_then(Value::as_str);
            let masked = ["Script file", "Script ID", "User ID"];
            Value::Object(
                (entries.iter())
                    .map(
                        |(key, value)| match title.filter(|title| masked.contains(title)) {
                            Some(_) if key == "value" => (key.clone(), json!("X")),
                            _ => (key.clone(), normal(value)),
                        },
                    )
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

fn cards(fake: &Fake) -> Vec<Value> {
    let requests = fake.requests();
    for request in &requests {
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/webhook/abc")
        );
        assert_eq!(request.header("Content-Type"), Some("application/json"));
    }
    requests
        .iter()
        .map(|request| normal(&request.json()))
        .collect()
}

#[test]
fn posts_cards() {
    let fake = Fake::start(json!([{"match": "/webhook", "status": 202, "body": "1"}]));
    assert_eq!(run("nf_tower", "main.nf", &fake, ALL), None);
    let expected: Vec<Value> = (recorded(include_str!("fixtures/nf_teams/req-success.log")).iter())
        .map(|request| normal(&request["body"]))
        .collect();
    assert_eq!(cards(&fake), expected);
}

#[test]
fn posts_failure_cards() {
    let fake = Fake::start(json!([{"match": "/webhook", "status": 202, "body": "1"}]));
    assert!(run("nf_teams", "fail.nf", &fake, ALL).is_some());
    let recorded: Vec<Value> = (recorded(include_str!("fixtures/nf_teams/req-fail.log")).iter())
        .map(|request| normal(&request["body"]))
        .collect();
    let cards = cards(&fake);
    assert_eq!(cards.len(), 3, "start, error and complete, each once");
    assert_eq!(cards[0], recorded[0]);
    assert_eq!(cards[2], recorded[1]);
    let error = &cards[1]["attachments"][0]["content"];
    let texts: Vec<&str> = (error["body"].as_array().into_iter().flatten())
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "Failed: demo/probe 1.0.0 - RUN",
            "Failed on X",
            "The pipeline failed because of the following issue:",
            "oops",
            "The command used to launch the workflow was as follows:",
            "COMMAND",
        ]
    );
    assert_eq!(error["body"][0]["color"], "attention");
    assert_eq!(
        error["msteams"]["summary"],
        "Failed: demo/probe 1.0.0 - RUN"
    );
}

#[test]
fn renders_custom_template() {
    let fake = Fake::start(json!([{"match": "/webhook", "status": 202, "body": "1"}]));
    let template = common::fixtures("nf_teams").join("start-custom.json");
    let events = format!(
        "  onStart {{ enabled = true; template = '{}' }}\n",
        template.display()
    );
    assert_eq!(run("nf_tower", "main.nf", &fake, &events), None);
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let user = std::env::var("USER").unwrap_or_default();
    // uwe knows the params of the script at the start, where Nextflow 26.04.6 has none yet.
    let text = format!("Run {RUN} of demo/probe by {user}, params 1");
    assert_eq!(requests[0].json(), json!({"type": "message", "text": text}));
}
