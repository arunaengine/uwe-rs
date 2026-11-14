//! The support report of small workflows of every language through the library call: which
//! parts uwe compiles, runs natively or refuses, and the images they name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};
use uwe::RunRequest;
use uwe::run::Settings;
use uwe::setup::Setup;
use uwe::support::Report;

/// The report of the workflow `name` among `files`, written into a new folder.
async fn report(files: &[(&str, &str)], name: &str) -> Report {
    let directory = tempfile::tempdir().expect("a temporary directory");
    for (file, text) in files {
        std::fs::write(directory.path().join(file), text).expect("writes a file");
    }
    let mut settings = Settings::new(directory.path().join(name));
    settings.setup = Some(Setup::default());
    settings.cache_folder = Some(directory.path().join("cache"));
    settings.database = Some(directory.path().join("runs.sqlite"));
    let report = RunRequest::new(settings).support().await.expect("a report");
    assert!(
        !directory.path().join("runs.sqlite").exists(),
        "records no run"
    );
    report
}

/// The parts of a report in their JSON form.
fn parts(report: &Report) -> Value {
    serde_json::to_value(&report.parts).expect("JSON parts")
}

/// The images of both processes come from parameters, one of them inside a string.
const PIPELINE: &str = "params.image = 'quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0'
params.tool = 'debian'

process QC {
    container params.image
    output:
    path 'q'
    script:
    \"echo q > q\"
}

process SAY {
    container \"${params.tool}:stable-slim\"
    output:
    path 'z'
    script:
    \"echo z > z\"
}

workflow {
    QC()
    SAY()
}
";

#[tokio::test]
async fn nextflow_parameter_images() {
    let report = report(&[("main.nf", PIPELINE)], "main.nf").await;
    assert!(report.runnable(), "{report:#?}");
    assert!(report.problems.is_empty(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([
            {"name": "QC", "support": "compiled", "location": "main.nf",
             "images": ["quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0"]},
            {"name": "SAY", "support": "compiled", "location": "main.nf",
             "images": ["debian:stable-slim"]}
        ])
    );
    let images: Vec<&str> = report.images().into_iter().collect();
    assert_eq!(
        images,
        [
            "debian:stable-slim",
            "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0"
        ]
    );
}

#[tokio::test]
async fn snakemake_rules() {
    let snakefile = "rule all:\n    input: \"out/a.txt\"\n\nrule make:\n    output: \
                     \"out/{name}.txt\"\n    container: \"docker://debian:stable-slim\"\n    \
                     shell: \"echo {wildcards.name} > {output}\"\n";
    let report = report(&[("Snakefile", snakefile)], "Snakefile").await;
    assert!(report.runnable(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([
            {"name": "make", "support": "compiled", "location": "Snakefile",
             "images": ["debian:stable-slim"]},
            {"name": "all", "support": "compiled", "location": "Snakefile"}
        ])
    );
}

/// A CWL workflow of one step whose tool names its image by digest.
const CWL: &str = "cwlVersion: v1.2
class: Workflow
inputs: []
outputs: []
steps:
  say:
    run:
      class: CommandLineTool
      baseCommand: echo
      requirements:
        DockerRequirement:
          dockerPull: debian@sha256:0000000000000000000000000000000000000000000000000000000000000000
      inputs: []
      outputs: []
    in: []
    out: []
";

const DIGEST: &str =
    "debian@sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn cwl_digest_images() {
    let report = report(&[("main.cwl", CWL)], "main.cwl").await;
    assert!(report.runnable(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([{"name": "say", "support": "compiled", "location": "main.cwl", "images": [DIGEST]}])
    );
}

/// A wir document whose one op runs the CWL workflow as a native block.
#[tokio::test]
async fn wir_native_blocks() {
    let document = json!({
        "wir_version": "0.1",
        "sources": [{"id": "block", "language": "cwl", "location": "main.cwl"}],
        "graphs": [{
            "name": "main",
            "ops": [{"path": "block", "kind": {"native": {"source": "block", "language": "cwl"}}}]
        }],
        "entry": "main"
    });
    let files = [("main.cwl", CWL), ("run.wir.json", &document.to_string())];
    let report = report(&files, "run.wir.json").await;
    assert!(report.runnable(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([{"name": "say", "support": "native", "location": "main.cwl", "images": [DIGEST]}])
    );
}

/// A task whose output reads what it prints stays native; an `else` clause is refused.
const WDL: &str = "version 1.3

task say {
  command <<< echo hi >>>
  runtime { container: \"debian:stable-slim\" }
  output { String out = read_string(stdout()) }
}

workflow main {
  input { Boolean flag = true }
  if (flag) {
    call say
  }
}
";

#[tokio::test]
async fn wdl_else_refused() {
    let report = report(&[("main.wdl", WDL)], "main.wdl").await;
    assert!(report.runnable(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([{
            "name": "main.say", "support": "native",
            "reason": "output out reads files that the task makes",
            "location": "main.wdl", "images": ["debian:stable-slim"]
        }])
    );
    let otherwise = WDL.replace(
        "    call say\n  }\n",
        "    call say\n  } else {\n    call say as other\n  }\n",
    );
    let report = self::report(&[("main.wdl", &otherwise)], "main.wdl").await;
    assert!(!report.runnable(), "{report:#?}");
    let reason = "main.wdl:11: `else` clauses are not supported";
    assert_eq!(
        parts(&report),
        json!([{"name": "main", "support": "unsupported", "reason": reason, "location": "main.wdl:11"}])
    );
    assert_eq!(
        serde_json::to_value(&report.problems).expect("JSON problems"),
        json!([{"kind": "unsupported", "message": reason}])
    );
}

#[tokio::test]
async fn argo_templates() {
    let workflow = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {generateName: \
                    x-}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    steps:\n    \
                    - - name: hello\n        template: say\n  - name: say\n    container:\n      \
                    image: alpine:3.21\n      command: [echo, hi]\n";
    let report = report(&[("workflow.yaml", workflow)], "workflow.yaml").await;
    assert!(report.runnable(), "{report:#?}");
    assert_eq!(
        parts(&report),
        json!([
            {"name": "say", "support": "compiled", "images": ["alpine:3.21"]},
            {"name": "hello", "support": "compiled", "location": "workflow.yaml:9:9"}
        ])
    );
}
