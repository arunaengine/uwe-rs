//! Tests of the pass that links a recorded tool and of its YAML text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

use super::*;
use serde_json::json;

fn input(id: &str, kind: Kind, default: Json, binding: Binding) -> Input {
    Input {
        id: id.to_owned(),
        kind,
        default: Some(default),
        binding,
    }
}

fn tool(inputs: Vec<Input>, outputs: Vec<Output>) -> Tool {
    Tool {
        base_command: vec!["tool".into()],
        arguments: Vec::new(),
        inputs,
        outputs,
        stdout: None,
        stderr: None,
        staged: Vec::new(),
        image: None,
    }
}

#[test]
fn references_named_inputs() {
    let file = json!({ "class": "File", "location": "in.txt" });
    let mut recorded = tool(
        vec![
            input("source", Kind::File, file, Binding::Position(0)),
            input(
                "output",
                Kind::String,
                json!("out.txt"),
                Binding::Prefix("-o".into()),
            ),
        ],
        vec![Output::found(Path::new("out.txt"), false)],
    );
    recorded.stdout = Some("in.txt".into());
    recorded.finish();
    assert_eq!(recorded.outputs[0].glob, "$(inputs.output)");
    assert_eq!(recorded.stdout.as_deref(), Some("$(inputs.source.path)"));
}

#[test]
fn makes_ids_unique() {
    let mut recorded = tool(
        vec![
            input(
                "out_txt",
                Kind::String,
                json!("a"),
                Binding::Prefix("-i".into()),
            ),
            input(
                "out_txt",
                Kind::String,
                json!("b"),
                Binding::Prefix("-i".into()),
            ),
        ],
        vec![Output::found(Path::new("out.txt"), false)],
    );
    recorded.finish();
    let ids: Vec<&str> = recorded
        .inputs
        .iter()
        .map(|input| input.id.as_str())
        .collect();
    assert_eq!(ids, ["out_txt", "out_txt_2"]);
    assert_eq!(recorded.outputs[0].id, "o_out_txt");
}

#[test]
fn names_by_path() {
    let found = Output::found(Path::new("results/2nd.plot.png"), false);
    assert_eq!(found.id, "results_2nd_plot_png");
    assert_eq!(found.format.as_deref(), Some("edam:format_3603"));
    let folder = Output::found(Path::new("results"), true);
    assert_eq!((folder.kind, folder.format), (Kind::Directory, None));
}

#[test]
fn renders_cwl_yaml() {
    let file = json!({ "class": "File", "location": "input.txt" });
    let mut recorded = tool(
        vec![input("input_txt", Kind::File, file, Binding::Position(0))],
        vec![Output::found(Path::new("output.txt"), false)],
    );
    recorded.base_command = vec!["cat".into()];
    recorded.stdout = Some("output.txt".into());
    recorded.image = Some("docker.io/library/busybox:1.37.0".into());
    recorded.finish();
    let expected = r#"#!/usr/bin/env cwl-runner
cwlVersion: v1.2
class: CommandLineTool
$namespaces:
  edam: "http://edamontology.org/"
hints:
  DockerRequirement:
    dockerPull: "docker.io/library/busybox:1.37.0"
baseCommand: [cat]
inputs:
  input_txt:
    type: File
    default:
      class: File
      location: input.txt
    inputBinding:
      position: 0
outputs:
  output_txt:
    type: File
    format: edam:format_2330
    outputBinding:
      glob: output.txt
stdout: output.txt
"#;
    assert_eq!(recorded.render(), expected);
}

#[test]
fn quotes_unsafe_scalars() {
    assert_eq!(scalar("plain-name_1.txt"), "plain-name_1.txt");
    assert_eq!(scalar("yes"), "\"yes\"");
    assert_eq!(scalar("1.5"), "\"1.5\"");
    assert_eq!(scalar("-l"), "\"-l\"");
    assert_eq!(scalar("$(inputs.a)"), "\"$(inputs.a)\"");
    assert_eq!(key("2"), "\"2\"");
}
