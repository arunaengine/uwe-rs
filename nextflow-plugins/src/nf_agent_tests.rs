//! Checks the credential lookup of nf-agent, the schemas of typed answers and the messages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::lowered::{AnswerField, AnswerType, Structured};
use serde_json::json;

use super::*;

fn options(provider: Option<&str>, base_url: Option<&str>) -> Options {
    Options {
        provider: provider.map(str::to_owned),
        key: None,
        base_url: base_url.map(str::to_owned),
        timeout: Duration::from_secs(1),
    }
}

/// The endpoint for a model with these environment variables, or the error.
fn resolve(options: &Options, model: &str, set: &[(&str, &str)]) -> Result<Endpoint, String> {
    let variable = |name: &str| {
        (set.iter())
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    };
    endpoint(options, model, &variable).map_err(|refused| match refused {
        Refused::Task(message) | Refused::Run(message) => message,
    })
}

#[test]
fn resolves_credentials() {
    let local = options(None, Some("http://127.0.0.1:8000/v1"));
    let found = resolve(&local, "openai/qwen", &[]).expect("a local endpoint");
    assert_eq!(
        (found.url.as_str(), found.key.as_str()),
        ("http://127.0.0.1:8000/v1", NO_KEY)
    );
    let key = [("OPENAI_API_KEY", "sk-test")];
    let found = resolve(&options(None, None), "openai/gpt-5-mini", &key).expect("OpenAI");
    assert_eq!(
        (found.url.as_str(), found.key.as_str()),
        (DEFAULT_URL, "sk-test")
    );
    let found = resolve(
        &options(None, None),
        "openai/m",
        &[("OPENAI_BASE_URL", "http://h/v1")],
    );
    assert_eq!(found.expect("its own endpoint").url, "http://h/v1");
    let withheld = resolve(&local, "openai/qwen", &key)
        .err()
        .unwrap_or_default();
    assert!(
        withheld.starts_with(
            "The `openai` credential resolved for agent model `openai/qwen` was withheld from \
             the endpoint http://127.0.0.1:8000/v1 - that endpoint is not one `openai` owns"
        ),
        "{withheld}"
    );
    let missing = resolve(&options(None, None), "openai/m", &[])
        .err()
        .unwrap_or_default();
    assert_eq!(
        missing,
        "Missing LLM provider credential for agent model `openai/m` - set `agent.apiKey` in \
         the Nextflow configuration, or the `OPENAI_API_KEY` environment variable; the \
         credential namespace is `openai`, set `agent.apiProvider` to name a different one"
    );
    let router = options(None, Some("https://openrouter.ai/api/v1"));
    let missing = resolve(&router, "openai/m", &[]).err().unwrap_or_default();
    assert!(
        missing.starts_with(
            "Missing `openrouter` credential for the agent endpoint https://openrouter.ai/api/v1 \
             - set `agent.apiKey` in the Nextflow configuration, or the `OPENROUTER_API_KEY`"
        ),
        "{missing}"
    );
    let crossed = Options {
        key: Some("k".into()),
        ..options(Some("anthropic"), Some("https://api.openai.com/v1"))
    };
    let found = resolve(&crossed, "openai/m", &[]).expect("the key of the config");
    assert!(found.warnings[0].starts_with(
        "Agent config sets `agent.apiProvider = 'anthropic'` but the endpoint \
         https://api.openai.com/v1 is a known `openai` endpoint"
    ));
}

#[test]
fn rejects_models() {
    let local = options(None, Some("http://127.0.0.1:1"));
    let error = resolve(&local, "gpt-5", &[]).err();
    assert_eq!(
        error.as_deref(),
        Some("Invalid model id `gpt-5` - expected `provider/model`")
    );
    let error = resolve(&local, "anthropic/claude", &[])
        .err()
        .unwrap_or_default();
    assert!(
        error.starts_with("Unsupported agent model provider `anthropic` - only the `openai`"),
        "{error}"
    );
}

#[test]
fn maps_answer_schemas() {
    let field = |name: &str, kind: AnswerType| AnswerField {
        name: Rc::from(name),
        kind,
        optional: false,
    };
    let record = Structured {
        name: Rc::from("result"),
        kind: AnswerType::Record(vec![
            field("summary", AnswerType::Text),
            field("confidence", AnswerType::Float),
            field("actionable", AnswerType::Boolean),
            field("key_points", AnswerType::List(Box::new(AnswerType::Text))),
        ]),
    };
    // The stored schema of this record in the lineage docs of the spec.
    let stored = "{\"additionalProperties\":false,\"properties\":{\"actionable\":{\"type\":\
                  \"boolean\"},\"confidence\":{\"type\":\"number\"},\"key_points\":{\"items\":\
                  {\"type\":\"string\"},\"type\":\"array\"},\"summary\":{\"type\":\"string\"}},\
                  \"required\":[\"summary\",\"confidence\",\"actionable\",\"key_points\"],\
                  \"type\":\"object\"}";
    let expected: Json = serde_json::from_str(stored).expect("JSON");
    assert_eq!(schema(&record), expected);
    let count = Structured {
        name: Rc::from("count"),
        kind: AnswerType::Integer,
    };
    let wrapped = json!({
        "type": "object",
        "properties": {"count": {"type": "integer"}},
        "required": ["count"],
        "additionalProperties": false,
    });
    assert_eq!(schema(&count), wrapped);
    let optional = Structured {
        name: Rc::from("result"),
        kind: AnswerType::Record(vec![
            field("label", AnswerType::Text),
            AnswerField {
                optional: true,
                ..field("note", AnswerType::Text)
            },
        ]),
    };
    let strict = strict(&schema(&optional));
    assert_eq!(strict["required"], json!(["label", "note"]));
    assert_eq!(
        strict["properties"]["note"]["type"],
        json!(["string", "null"])
    );
}

#[test]
fn builds_messages() {
    assert_eq!(system("Be brief.", ""), "Be brief.");
    assert_eq!(
        system("Be brief.", "Name the cause."),
        "Be brief.\n\nGoal:\nName the cause.\nYou are done when the goal is met; produce your \
         final answer as plain text."
    );
    let input = json!({"id": "s1", "reads": "a.fq"});
    assert_eq!(
        user("Check it.", Some(&input)),
        "Check it.\n\nInput (JSON):\n{\"id\":\"s1\",\"reads\":\"a.fq\"}"
    );
    let first = cache_key(&json!({"model": "a"}));
    assert_eq!(first.len(), 64);
    assert_ne!(first, cache_key(&json!({"model": "b"})));
}
