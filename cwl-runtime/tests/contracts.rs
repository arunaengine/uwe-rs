//! Checks CWL output, schema, extension and software admission contracts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options, Value};
use workflow_graph::{Driver, Launch, Outcome, OutputFile};

fn loaded(source: &str, job: &str, options: Options) -> (tempfile::TempDir, CwlDriver) {
    let root = tempfile::tempdir().expect("folder");
    std::fs::write(root.path().join("main.cwl"), source).expect("source");
    std::fs::write(root.path().join("job.json"), job).expect("job");
    let launch = Launch {
        target: root.path().join("main.cwl"),
        inputs: Some(root.path().join("job.json")),
        scratch: root.path().join("scratch"),
        ..Launch::default()
    };
    let driver = CwlDriver::new(&launch, options).expect("driver");
    (root, driver)
}

#[test]
fn honors_json_override() {
    let source = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\ninputs: []\noutputs:\n  out: {type: File, outputBinding: {glob: old.txt}}\n";
    let (root, mut driver) = loaded(
        source,
        "{}",
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    let tasks = driver.start().expect("start");
    assert!(
        tasks[0]
            .outputs
            .iter()
            .any(|path| path == "cwl.output.json")
    );
    assert_eq!(tasks[0].output_manifest.as_deref(), Some("cwl.output.json"));
    assert!(
        !tasks[0]
            .outputs
            .iter()
            .any(|path| matches!(path.as_str(), "." | "*"))
    );
    std::fs::write(root.path().join("old.txt"), "old bytes").expect("old");
    std::fs::write(root.path().join(".new.txt"), "selected bytes").expect("selected");
    std::fs::write(
        root.path().join("cwl.output.json"),
        r#"{"out":{"class":"File","path":".new.txt"}}"#,
    )
    .expect("override");
    driver
        .complete(Outcome {
            id: tasks[0].id,
            files: vec![
                OutputFile {
                    path: "old.txt".into(),
                    location: root.path().join("old.txt").to_string_lossy().into_owned(),
                    ..OutputFile::default()
                },
                OutputFile {
                    path: ".new.txt".into(),
                    location: root.path().join(".new.txt").to_string_lossy().into_owned(),
                    ..OutputFile::default()
                },
                OutputFile {
                    path: "cwl.output.json".into(),
                    location: root
                        .path()
                        .join("cwl.output.json")
                        .to_string_lossy()
                        .into_owned(),
                    ..OutputFile::default()
                },
            ],
            ..Outcome::default()
        })
        .expect("completion");
    let out = &driver.outputs().expect("outputs")["out"];
    let path =
        cwl_runtime::files::local_path(out.text("location").expect("location")).expect("path");
    assert_eq!(
        std::fs::read_to_string(path).expect("selected output"),
        "selected bytes"
    );
}

#[test]
fn checks_expression_outputs() {
    for (kind, data, accepted) in [
        ("int", r#"{"out":"wrong"}"#, false),
        ("int", "{}", false),
        ("int?", "{}", true),
        ("int", r#"{"out":7}"#, true),
    ] {
        let source = format!(
            "cwlVersion: v1.2\nclass: ExpressionTool\ninputs: {{data: Any}}\noutputs: {{out: '{kind}'}}\nexpression: $(inputs.data)\n"
        );
        let (_root, mut driver) = loaded(
            &source,
            &format!(r#"{{"data":{data}}}"#),
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        assert_eq!(driver.start().is_ok(), accepted, "{kind}: {data}");
        assert_eq!(driver.outputs().is_some(), accepted);
    }
}

#[test]
fn preserves_nullable_items() {
    let source = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\ninputs:\n  values: {type: array, items: ['null', string]}\n  nested: {type: array, items: {type: array, items: ['null', int]}}\noutputs: []\n";
    let (_root, mut driver) = loaded(
        source,
        r#"{"values":[null,"x"],"nested":[[null,2],[]]}"#,
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    let mut inputs = driver.inputs().expect("schema").expect("inputs");
    assert!(
        input_schema::validate_json(&inputs, false)
            .diagnostics
            .is_empty()
    );
    inputs.values["nested"][0][1] = serde_json::json!("bad");
    assert!(
        !input_schema::validate_json(&inputs, false)
            .diagnostics
            .is_empty()
    );
}

#[test]
fn checks_extension_identity() {
    for (class, accepted) in [
        ("http://arvados.org/cwl#ProcessProperties", true),
        ("https://example.org/custom#ProcessProperties", false),
        ("http://commonwl.org/cwltool#CUDARequirement", true),
        ("https://example.org/custom#CUDARequirement", false),
        ("http://commonwl.org/cwltool#ShmSize", true),
        ("https://example.org/custom#ShmSize", false),
    ] {
        let source = format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\nrequirements: [{{class: '{class}'}}]\ninputs: []\noutputs: []\n"
        );
        let (_root, mut driver) = loaded(
            &source,
            "{}",
            Options {
                simulated: false,
                shm: true,
                ..Options::default()
            },
        );
        let result = driver.start();
        assert_eq!(result.is_ok(), accepted, "{class}: {result:?}");
        if !accepted {
            assert!(result.unwrap_err().unsupported);
        }
    }
}

#[test]
fn prepares_required_software() {
    let source = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: samtools\nrequirements:\n  SoftwareRequirement:\n    packages:\n    - package: samtools\n      version: ['1.19', '1.20']\ninputs: []\noutputs: []\n";
    for enabled in [false, true] {
        let (_root, mut driver) = loaded(
            source,
            "{}",
            Options {
                simulated: false,
                software: enabled,
                ..Options::default()
            },
        );
        let result = driver.start();
        if enabled {
            let task = result.expect("prepared").remove(0);
            let source = task.image.expect("image");
            let packages: String = serde_json::from_str(
                source
                    .strip_prefix("conda-env:nextflow-packages:")
                    .expect("native resolver"),
            )
            .expect("packages");
            assert_eq!(packages, "samtools[version='==1.19|==1.20']");
        } else {
            assert!(result.expect_err("unconfigured resolver").unsupported);
        }
    }
}

#[test]
fn bounds_scatter_admission() {
    let source = "cwlVersion: v1.2\nclass: Workflow\nrequirements: {ScatterFeatureRequirement: {}}\ninputs: {values: 'int[]'}\noutputs: {out: {type: 'int[]', outputSource: each/out}}\nsteps:\n  each:\n    scatter: x\n    in: {x: values}\n    out: [out]\n    run:\n      class: CommandLineTool\n      baseCommand: 'true'\n      inputs: {x: int}\n      outputs: {out: {type: int, outputBinding: {outputEval: '$(inputs.x)'}}}\n";
    let values: Vec<i64> = (0..1000).collect();
    let (_root, mut driver) = loaded(
        source,
        &serde_json::json!({"values": values}).to_string(),
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    let mut pending = driver.start().expect("start");
    assert_eq!(pending.len(), 64);
    let mut finished = 0;
    while let Some(task) = pending.pop() {
        let ready = driver
            .complete(Outcome {
                id: task.id,
                ..Outcome::default()
            })
            .expect("completion");
        pending.extend(ready);
        assert!(pending.len() <= 64);
        finished += 1;
    }
    assert_eq!(finished, 1000);
    assert_eq!(
        driver.outputs().expect("outputs")["out"],
        Value::List(values.into_iter().map(Value::Integer).collect())
    );
}

#[test]
fn preserves_scatter_order() {
    for (method, expected) in [
        ("dotproduct", "[{x: 1, y: 3}, {x: 2, y: 4}]"),
        (
            "flat_crossproduct",
            "[{x: 1, y: 3}, {x: 1, y: 4}, {x: 2, y: 3}, {x: 2, y: 4}]",
        ),
        (
            "nested_crossproduct",
            "[[{x: 1, y: 3}, {x: 1, y: 4}], [{x: 2, y: 3}, {x: 2, y: 4}]]",
        ),
    ] {
        let source = format!(
            "cwlVersion: v1.2\nclass: Workflow\nrequirements: {{ScatterFeatureRequirement: {{}}}}\ninputs: {{xs: 'int[]', ys: 'int[]'}}\noutputs: {{out: {{type: Any, outputSource: each/out}}}}\nsteps:\n  each:\n    scatter: [x, y]\n    scatterMethod: {method}\n    in: {{x: xs, y: ys}}\n    out: [out]\n    run:\n      class: CommandLineTool\n      baseCommand: 'true'\n      inputs: {{x: int, y: int}}\n      outputs: {{out: {{type: Any, outputBinding: {{outputEval: '$(inputs)'}}}}}}\n"
        );
        let (_root, mut driver) = loaded(
            &source,
            r#"{"xs":[1,2],"ys":[3,4]}"#,
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        let mut pending = driver.start().expect("start");
        while let Some(task) = pending.pop() {
            pending.extend(
                driver
                    .complete(Outcome {
                        id: task.id,
                        ..Outcome::default()
                    })
                    .expect("completion"),
            );
        }
        assert_eq!(
            driver.outputs().expect("outputs")["out"],
            Value::parse(expected).expect("expected"),
            "{method}"
        );
        let (_root, mut driver) = loaded(
            &source,
            r#"{"xs":[],"ys":[]}"#,
            Options {
                simulated: false,
                ..Options::default()
            },
        );
        assert!(driver.start().expect("empty scatter").is_empty());
        assert_eq!(
            driver.outputs().expect("empty outputs")["out"],
            Value::List(Vec::new())
        );
    }
}

#[test]
fn limits_json_retention() {
    let root = tempfile::tempdir().expect("folder");
    let reference = root.path().join("reference.fa");
    std::fs::File::create(&reference)
        .expect("reference")
        .set_len(1 << 30)
        .expect("sparse reference");
    let source = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\nrequirements:\n  InitialWorkDirRequirement:\n    listing: ['$(inputs.reference)']\ninputs: {reference: File}\noutputs: {out: File}\n";
    let job = serde_json::json!({"reference":{"class":"File","location": cwl_parser::iri::file_url(&reference)}}).to_string();
    let (_root, mut driver) = loaded(
        source,
        &job,
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    let tasks = driver.start().expect("task");
    assert_eq!(tasks[0].outputs, ["cwl.output.json"]);
    assert_eq!(tasks[0].output_manifest.as_deref(), Some("cwl.output.json"));
    assert!(
        tasks[0]
            .inputs
            .iter()
            .any(|input| input.source == reference.to_string_lossy())
    );
}

#[test]
fn preserves_readonly_aliases() {
    let source = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\nrequirements:\n  InitialWorkDirRequirement:\n    listing: ['$(inputs.reference)']\ninputs: {reference: File}\noutputs: {out: File}\n";
    let input = tempfile::tempdir().expect("input folder");
    let reference = input.path().join("reference.fa");
    std::fs::write(&reference, "reference bytes").expect("reference");
    let job = serde_json::json!({"reference":{"class":"File","location": cwl_parser::iri::file_url(&reference)}}).to_string();
    let (root, mut driver) = loaded(
        source,
        &job,
        Options {
            simulated: false,
            ..Options::default()
        },
    );
    let task = driver.start().expect("start").remove(0);
    assert!(task.inputs.iter().any(
        |input| input.path == "reference.fa" && input.writable == workflow_graph::Writable::No
    ));
    let manifest = root.path().join("cwl.output.json");
    std::fs::write(
        &manifest,
        r#"{"out":{"class":"File","path":"reference.fa"}}"#,
    )
    .expect("manifest");
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "cwl.output.json".into(),
                location: manifest.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .expect("complete");
    assert_eq!(
        driver.outputs().expect("outputs")["out"].text("location"),
        Some(cwl_parser::iri::file_url(&reference).as_str())
    );
}

#[test]
fn maps_absolute_aliases() {
    for directory in [false, true] {
        for writable in [false, true] {
            let input = tempfile::tempdir().unwrap();
            let original = input.path().join("original");
            if directory {
                std::fs::create_dir(&original).unwrap();
            }
            let leaf = if directory {
                original.join("child")
            } else {
                original.clone()
            };
            std::fs::write(&leaf, "before").unwrap();
            let class = if directory { "Directory" } else { "File" };
            let source = format!(
                "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\nrequirements:\n  DockerRequirement: {{dockerPull: debian:stable-slim}}\n  InitialWorkDirRequirement:\n    listing: [{{entry: '$(inputs.data)', entryname: /scratch/result, writable: {writable}}}]\ninputs: {{data: {class}}}\noutputs: {{out: File}}\n"
            );
            let job = serde_json::json!({"data":{"class":class,"location":cwl_parser::iri::file_url(&original)}}).to_string();
            let (root, mut driver) = loaded(
                &source,
                &job,
                Options {
                    simulated: false,
                    ..Options::default()
                },
            );
            let task = driver.start().unwrap().remove(0);
            let selected = if directory {
                "/scratch/./result/child"
            } else {
                "/scratch/./result"
            };
            let manifest = root.path().join("cwl.output.json");
            std::fs::write(
                &manifest,
                serde_json::json!({"out":{"class":"File","path":selected}}).to_string(),
            )
            .unwrap();
            let mut files = vec![OutputFile {
                path: "cwl.output.json".into(),
                location: manifest.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }];
            let updated = root.path().join("updated");
            let target = if directory {
                std::fs::create_dir(&updated).unwrap();
                updated.join("child")
            } else {
                updated.clone()
            };
            if writable {
                std::fs::write(&target, "after").unwrap();
                files.push(OutputFile {
                    path: if directory {
                        "/scratch/result/"
                    } else {
                        "/scratch/result"
                    }
                    .into(),
                    location: updated.to_string_lossy().into_owned(),
                    directory,
                    ..OutputFile::default()
                });
            }
            driver
                .complete(Outcome {
                    id: task.id,
                    files,
                    ..Outcome::default()
                })
                .unwrap();
            let expected = if writable { &target } else { &leaf };
            assert_eq!(
                driver.outputs().unwrap()["out"].text("location"),
                Some(cwl_parser::iri::file_url(expected).as_str())
            );
            assert_eq!(std::fs::read_to_string(&leaf).unwrap(), "before");
        }
    }
}
