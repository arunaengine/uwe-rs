//! Signature capture reads supported values without invoking unused Python callbacks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Files, Launch, LocalFiles, Outcome, OutputFile};

#[test]
fn rejects_uncomparable_params() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("Snakefile"), r#"
seen = []
class Unused:
    def __repr__(self):
        seen.append("called")
        raise ValueError("unused repr must not execute")
cyclic = []
cyclic.append(cyclic)
rule make:
    output: "out.txt"
    params: custom=lambda wildcards: Unused(), cyclic=lambda wildcards: cyclic, values=lambda wildcards: [2, "text", {"b": 3, "a": 1}], numbers=lambda wildcards: {3, 1}
    shell: f"echo {len(seen)}"
"#).unwrap();
    let mut driver = snakemake_runtime::driver(&Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let task = driver.start().unwrap().remove(0);
    assert_eq!(task.script.as_deref(), Some("echo 0"));
    let source = folder.path().join("raw");
    std::fs::write(&source, "made").unwrap();
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "out.txt".into(),
                location: source.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .unwrap();
    for delivery in driver.deliveries() {
        LocalFiles.deliver(&delivery).unwrap();
        driver.delivered(&delivery).unwrap();
    }
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(folder.path().join(".snakemake/uwe-signatures/b3V0LnR4dA==")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["params"][0], serde_json::Value::Null);
    assert_eq!(value["params"][1], serde_json::Value::Null);
    assert_eq!(
        value["params"][2],
        serde_json::json!([
            "list",
            [
                ["int", "2"],
                ["str", "text"],
                [
                    "dict",
                    [
                        ["tuple", [["str", "a"], ["int", "1"]]],
                        ["tuple", [["str", "b"], ["int", "3"]]],
                    ]
                ],
            ]
        ])
    );
    assert_eq!(
        value["params"][3],
        serde_json::json!(["set", [["int", "1"], ["int", "3"]]])
    );
    driver.finish().unwrap();
    let mut unchanged = snakemake_runtime::driver(&Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    assert!(unchanged.start().unwrap().is_empty());
    unchanged.finish().unwrap();
}
