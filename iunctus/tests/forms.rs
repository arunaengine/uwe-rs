//! Form descriptions preserve complete schemas and validate posted typed JSON.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use iunctus::application::{ApplicationError, Description, Policy};
use iunctus::forms::{self, Form};
use serde_json::{Value, json};
use workflow_graph::{Driver, DriverError, Inputs, Language, Launch, Outcome, Task};

struct Described(Inputs);

impl Driver for Described {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        panic!("form discovery and validation must not start tasks")
    }

    fn complete(&mut self, _outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        panic!("form discovery and validation must not execute tasks")
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        panic!("form discovery and validation must not execute tasks")
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        Ok(Some(self.0.clone()))
    }
}

fn inputs() -> Inputs {
    Inputs {
        schema: json!({
            "$schema": input_schema::DRAFT,
            "type": "object",
            "$defs": {"count": {"type": "integer", "minimum": 1}},
            "allOf": [{"required": ["count"]}],
            "properties": {
                "count": {"$ref": "#/$defs/count"},
                "a/b~": {"oneOf": [{"type": "integer"}, {"type": "boolean"}]},
                "defaulted": {"type": "string", "default": "annotation"}
            }
        }),
        values: json!({"count": 2}),
        ..Inputs::default()
    }
}

#[test]
fn preserves_schema_values() {
    let inputs = inputs();
    let mut driver = Described(inputs.clone());
    let form = forms::describe(&mut driver, Policy::Strict).expect("describes a form");
    let shown = form.json();

    assert_eq!(shown["version"], json!(1));
    assert_eq!(shown["language"], json!("nextflow"));
    assert_eq!(shown["schema"], inputs.schema);
    assert_eq!(shown["values"], inputs.values);
    assert_eq!(shown["validation"]["values"], json!("typed-json"));
    assert_eq!(shown["validation"]["defaults"], json!("annotations"));
    assert_eq!(shown["validation"]["remote_content_checked"], json!(false));
}

#[test]
fn preserves_wire_bytes() {
    let form = forms::describe(&mut Described(inputs()), Policy::Strict).expect("a form");
    let shown = form.response();
    let bytes = serde_json::to_vec(&shown).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&form.json()).unwrap());
    assert_eq!(
        serde_json::from_slice::<forms::FormResponse>(&bytes).unwrap(),
        shown
    );
    for values in [json!({"count": 2}), json!({"count": "wrong", "a/b~": []})] {
        let response = form.validate_response(values.clone());
        let bytes = serde_json::to_vec(&response).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&form.validate(values)).unwrap());
        assert_eq!(
            serde_json::from_slice::<forms::ValidationResponse>(&bytes).unwrap(),
            response
        );
    }
    let unavailable = forms::ValidationResponse::Unavailable(forms::UnavailableResponse {
        accepted: false,
        error: "input.unavailable".into(),
        message: "unavailable".into(),
        version: forms::FORMAT,
    });
    let bytes = serde_json::to_vec(&unavailable).unwrap();
    assert_eq!(
        bytes,
        br#"{"accepted":false,"error":"input.unavailable","message":"unavailable","version":1}"#
    );
    assert_eq!(
        serde_json::from_slice::<forms::ValidationResponse>(&bytes).unwrap(),
        unavailable
    );
}

#[test]
fn validates_posted_types() {
    let form = forms::describe(&mut Described(inputs()), Policy::Strict).expect("a form");
    let values = json!({"count": "2", "a/b~": []});
    let rejected = form.validate(values.clone());

    assert_eq!(rejected["accepted"], json!(false));
    assert_eq!(rejected["error"], json!("input.invalid"));
    assert_eq!(rejected["values"], values);
    let errors = rejected["diagnostics"].as_array().expect("field errors");
    assert!(
        errors
            .iter()
            .any(|error| error["instance_path"] == "/count")
    );
    let choice = errors
        .iter()
        .find(|error| error["instance_path"] == "/a~1b~0")
        .expect("an escaped pointer");
    assert_eq!(choice["branches"].as_array().expect("branches").len(), 2);
    let approved = form.validate(json!({"count": 2, "a/b~": true}));
    assert_eq!(approved["accepted"], json!(true));
    assert!(approved["error"].is_null());
    assert!(approved["values"].get("defaulted").is_none());
    assert_eq!(form.json()["values"], json!({"count": 2}));
    assert_eq!(form.validate(json!({}))["accepted"], json!(false));
}

#[test]
fn enforces_schema_policy() {
    assert!(matches!(
        Form::new(
            Description {
                language: Language::Cwl,
                inputs: None,
                outputs: None,
            },
            Policy::Relaxed,
        ),
        Err(ApplicationError::MissingSchema { .. })
    ));
    let mut advisory = inputs();
    advisory.advisory = true;
    advisory.assumes_values = true;
    let strict = forms::describe(&mut Described(advisory.clone()), Policy::Strict).expect("a form");
    let relaxed = forms::describe(&mut Described(advisory), Policy::Relaxed).expect("a form");

    assert_eq!(
        strict.validate(json!({"count": 2}))["accepted"],
        json!(false)
    );
    assert_eq!(
        strict.validate(json!({"count": 2}))["error"],
        json!("input.advisory_schema")
    );
    let warning = relaxed.validate(json!({"count": "wrong"}));
    assert_eq!(warning["accepted"], json!(true));
    assert_eq!(warning["diagnostics"][0]["severity"], json!("warning"));
}

#[test]
fn describes_native_forms() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let files = [
        (
            Language::Nextflow,
            "main.nf",
            "params.count = 3\nworkflow { }\n",
        ),
        (
            Language::Snakemake,
            "Snakefile",
            "config = {'count': 3}\nrule all:\n    input: []\n",
        ),
        (
            Language::Cwl,
            "main.cwl",
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs:\n  count:\n    type: int\n    default: 3\noutputs: []\n",
        ),
        (
            Language::Argo,
            "main.yaml",
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata:\n  name: form\nspec:\n  entrypoint: main\n  arguments:\n    parameters:\n      - name: count\n        value: '3'\n  templates:\n    - name: main\n      inputs:\n        parameters:\n          - name: count\n      container:\n        image: alpine:3.23\n        command: [echo]\n        args: ['{{inputs.parameters.count}}']\n",
        ),
    ];
    for (language, name, text) in files {
        let target = root.path().join(name);
        std::fs::write(&target, text).expect("writes the workflow");
        let launch = Launch {
            target,
            simulated: true,
            ..Launch::default()
        };
        let mut driver = match language {
            Language::Nextflow => nextflow_runtime::driver(&launch),
            Language::Snakemake => snakemake_runtime::driver(&launch),
            Language::Cwl => cwl_runtime::driver(&launch),
            Language::Argo => argo_runtime::driver(&launch),
        }
        .expect("a native driver");
        let form = forms::describe(driver.as_mut(), Policy::Relaxed).expect("a native form");
        let shown = form.json();
        assert!(input_schema::valid(&shown["schema"]).is_ok(), "{language}");
        assert_eq!(shown["language"], Value::from(language.to_string()));
        assert_eq!(
            form.validate(shown["values"].clone())["accepted"],
            json!(true),
            "{language}"
        );
    }
}
