//! The inputs and outputs of Nextflow pipelines as JSON Schema (decision 24).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use workflow_graph::Launch;

const SCRIPT: &str = r#"
params.reads = "data/*.fq"

process COUNT {
    input: path reads
    output: path 'counts.txt'
    script: "wc -l $reads > counts.txt"
}

workflow {
    main:
    counts = COUNT(channel.fromPath(params.reads))
    if (params.skip_report) { println params.label }

    publish:
    counts = counts
}

output {
    counts {
        path 'counts'
        index { path 'counts.csv' }
    }
}
"#;

#[test]
fn a_pipeline_without_a_schema_file_declares_its_params_and_outputs() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(
        folder.path().join("nextflow.config"),
        "manifest { name = 'counter' }\nparams { threads = 2\n skip_report = false }\n",
    )
    .expect("writes the config");
    let launch = Launch {
        target: folder.path().to_owned(),
        params: vec![("threads".into(), "4".into())],
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    let properties = &inputs.schema["properties"];
    assert_eq!(inputs.schema["title"], "counter");
    assert_eq!(
        properties["threads"],
        json!({"type": "integer", "default": 2})
    );
    assert_eq!(
        properties["reads"],
        json!({"type": "string", "default": "data/*.fq"})
    );
    assert_eq!(
        properties["skip_report"],
        json!({"type": "boolean", "default": false})
    );
    assert!(inputs.advisory);
    // Texts of the command line read as numbers: no problem.
    assert_eq!(input_schema::check(&inputs, false), []);
    let read = inputs.read.expect("the params that the code reads");
    assert!(read.contains("label") && read.contains("skip_report") && !read.contains("threads"));
    assert_eq!(
        input_schema::compare(&inputs.schema, &read),
        [
            "the workflow code reads an input that the schema does not declare: `label`",
            "the schema declares an input that the workflow code never reads: `threads`"
        ]
    );
    let outputs = driver.outputs().expect("describes").expect("outputs");
    assert_eq!(
        outputs["properties"]["counts"]["description"],
        "the files of the output `counts`, published to `results/counts`; `results/counts.csv` \
         lists them"
    );
    assert_eq!(input_schema::valid(&outputs), Ok(()));
}

#[test]
fn a_schema_file_keeps_its_groups_and_embeds_its_sample_sheet() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::create_dir(folder.path().join("assets")).expect("a folder");
    std::fs::write(
        folder.path().join("main.nf"),
        "workflow { println params.input }\n",
    )
    .expect("writes the script");
    std::fs::write(
        folder.path().join("nextflow.config"),
        "params { input = null }\n",
    )
    .expect("writes the config");
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema",
        "definitions": {"io": {"title": "Input", "required": ["input"], "properties": {
            "input": {"type": "string", "format": "file-path", "schema": "assets/schema_input.json"}
        }}},
        "allOf": [{"$ref": "#/definitions/io"}]
    });
    std::fs::write(
        folder.path().join("nextflow_schema.json"),
        schema.to_string(),
    )
    .expect("writes the schema");
    let rows = json!({"type": "array", "items": {"type": "object", "properties": {"sample": {"type": "string"}}}});
    std::fs::write(
        folder.path().join("assets/schema_input.json"),
        rows.to_string(),
    )
    .expect("writes the sample sheet schema");
    let launch = Launch {
        target: folder.path().to_owned(),
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    let input = &inputs.schema["$defs"]["io"]["properties"]["input"];
    assert_eq!(input["schema"], rows);
    assert_eq!(inputs.schema["allOf"], json!([{"$ref": "#/$defs/io"}]));
    // No code checks the params, so the missing input is a warning.
    assert!(inputs.advisory);
    let problems = input_schema::check(&inputs, false);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].severity, input_schema::Severity::Warning);
    assert_eq!(input_schema::valid(&inputs.schema), Ok(()));
}
