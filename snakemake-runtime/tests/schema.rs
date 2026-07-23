//! The inputs and outputs of Snakemake workflows as JSON Schema (decision 24).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use workflow_graph::Launch;

const SNAKEFILE: &str = r#"
import pandas as pd
from snakemake.utils import validate

configfile: "config.yaml"
validate(config, "config.schema.yaml")
samples = pd.read_csv(config["samples"], sep="\t")
validate(samples, "samples.schema.yaml")

rule all:
    input:
        report="results/report.html",
        counts=expand("results/{sample}.txt", sample=samples["sample"])

rule count:
    output: "results/{sample}.txt"
    shell: "touch {output}"

rule report:
    input: expand("results/{sample}.txt", sample=samples["sample"])
    output: "results/report.html"
    shell: "cat {input} > {output} {config[threads]}"
"#;

#[test]
fn a_workflow_describes_its_config_sample_sheet_and_targets() {
    let folder = tempfile::tempdir().expect("a folder");
    let files = [
        ("Snakefile", SNAKEFILE),
        ("config.yaml", "samples: samples.tsv\nthreads: 2\n"),
        (
            "config.schema.yaml",
            "$schema: http://json-schema.org/draft-07/schema#\nproperties:\n  samples: {type: string}\n  threads: {type: integer}\nrequired: [samples]\n",
        ),
        ("samples.tsv", "sample\tfastq\nA\ta.fq\nB\tb.fq\n"),
        (
            "samples.schema.yaml",
            "properties:\n  sample: {type: string}\n  fastq: {type: string}\nrequired: [sample]\n",
        ),
    ];
    for (name, text) in files {
        std::fs::write(folder.path().join(name), text).expect("writes a file");
    }
    let launch = Launch {
        target: folder.path().to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    let samples = &inputs.schema["properties"]["samples"];
    assert_eq!(samples["format"], "file-path");
    assert_eq!(samples["schema"]["items"]["required"], json!(["sample"]));
    assert_eq!(inputs.values["threads"], 2);
    assert_eq!(
        inputs.read,
        Some(["samples".to_owned(), "threads".to_owned()].into())
    );
    assert_eq!(input_schema::check(&inputs, false), []);
    let outputs = driver.outputs().expect("describes").expect("outputs");
    assert_eq!(
        outputs["properties"]["report"],
        json!({"type": "string", "format": "file-path", "const": "results/report.html"})
    );
    assert_eq!(
        outputs["properties"]["counts"]["const"],
        json!(["results/A.txt", "results/B.txt"])
    );
    assert_eq!(input_schema::valid(&outputs), Ok(()));
}
