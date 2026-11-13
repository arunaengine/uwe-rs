//! In-place writers defer expression evaluation and staging of subsequent work.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task, Writable};

const WRITER: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: update\n\
    requirements:\n  InitialWorkDirRequirement:\n    listing: [{entry: $(inputs.f), writable: true}]\n  \
    InplaceUpdateRequirement: {inplaceUpdate: true}\n\
    inputs: {f: {type: File, loadContents: true}}\noutputs: []\narguments: [$(inputs.f.contents), $(inputs.f.size)]\n";
const READER: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: read\n\
    inputs: {content: string}\noutputs: []\narguments: [$(inputs.content)]\n";
const READ_INPUT: &str =
    "{content: {source: data, loadContents: true, valueFrom: $(self.contents)}}";

fn workflow(steps: &str, scattered: bool) -> String {
    format!(
        "cwlVersion: v1.2\nclass: Workflow\nrequirements:\n  InlineJavascriptRequirement: {{}}\n  \
         StepInputExpressionRequirement: {{}}\n  ScatterFeatureRequirement: {{}}\n  SubworkflowFeatureRequirement: {{}}\n\
         inputs: {{data: {{type: '{}', loadContents: true}}}}\noutputs: []\nsteps:\n{steps}",
        if scattered { "File[]" } else { "File" }
    )
}

fn start(root: &Path, main: &str, scattered: bool, keep_going: bool) -> (CwlDriver, Vec<Task>) {
    std::fs::write(root.join("main.cwl"), main).expect("workflow");
    std::fs::write(root.join("writer.cwl"), WRITER).expect("writer");
    std::fs::write(root.join("reader.cwl"), READER).expect("reader");
    let input = root.join("data.txt");
    std::fs::write(&input, "seed").expect("input");
    let value = serde_json::json!({"class": "File", "location": cwl_parser::iri::file_url(&input)});
    let data = if scattered {
        serde_json::json!([value.clone(), value])
    } else {
        value
    };
    std::fs::write(
        root.join("job.json"),
        serde_json::json!({"data": data}).to_string(),
    )
    .expect("job");
    let launch = Launch {
        target: root.join("main.cwl"),
        inputs: Some(root.join("job.json")),
        scratch: root.join("scratch"),
        keep_going,
        ..Launch::default()
    };
    let config = config_file::ConfigFile::parse(
        &format!(
            "[storage.mirror]\ntype = \"folder\"\nprefixes = [\"s3://mirror/\"]\npath = \"{}\"\n",
            root.display()
        ),
        Path::new("config.toml"),
    )
    .expect("provider config");
    let storage = storage::Storage::from_config(&config).expect("provider");
    let mut driver = CwlDriver::new(
        &launch,
        Options {
            simulated: false,
            remote: cwl_parser::source::Remote::new(&storage, root.join("downloads")),
            ..Options::default()
        },
    )
    .expect("driver");
    let tasks = driver.start().expect("starts");
    (driver, tasks)
}

fn written(root: &Path, task: &Task, content: &str) -> Outcome {
    let path = root.join(task.id.to_string()).join("data.txt");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("work");
    std::fs::write(&path, content).expect("updated copy");
    Outcome {
        id: task.id,
        files: vec![OutputFile {
            path: "data.txt".into(),
            location: path.to_string_lossy().into_owned(),
            ..OutputFile::default()
        }],
        ..Outcome::default()
    }
}

#[test]
fn refreshes_deferred_inputs() {
    for remote in [false, true] {
        let root = tempfile::tempdir().expect("folder");
        let main = workflow(
            &format!(
                "  first: {{run: writer.cwl, in: {{f: data}}, out: []}}\n  \
             second: {{run: writer.cwl, in: {{f: data}}, out: []}}\n  \
             third: {{run: reader.cwl, in: {READ_INPUT}, out: [], when: '$(inputs.content != \"seed\")'}}\n"
            ),
            false,
        );
        let (mut driver, first) = start(root.path(), &main, false, false);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].command, ["update", "seed", "4"]);
        let mut outcome = written(root.path(), &first[0], "first update");
        if remote {
            outcome.files[0].location = format!("s3://mirror/{}/data.txt", first[0].id);
        }
        let location = outcome.files[0].location.clone();
        let second = driver.complete(outcome).expect("first completes");
        assert_eq!(second.len(), 1, "writer preparation: {:?}", driver.finish());
        assert_eq!(second[0].command, ["update", "first update", "12"]);
        let copy = second[0]
            .inputs
            .iter()
            .find(|input| input.writable == Writable::Copy)
            .expect("writable copy");
        assert_eq!(copy.source, location);
        let read = driver
            .complete(written(root.path(), &second[0], "second update"))
            .expect("second completes");
        assert_eq!(read.len(), 1, "when must see refreshed content");
        assert_eq!(read[0].command, ["read", "second update"]);
        assert!(
            driver
                .complete(Outcome {
                    id: read[0].id,
                    ..Outcome::default()
                })
                .expect("read completes")
                .is_empty()
        );
        assert!(driver.outputs().is_some());
        assert_eq!(
            std::fs::read_to_string(root.path().join("data.txt")).expect("source"),
            "seed"
        );
    }
}

#[test]
fn refreshes_file_aliases() {
    for hard_link in [false, true] {
        let root = tempfile::tempdir().expect("folder");
        let original = root.path().join("data.txt");
        let alias = root.path().join("alias.txt");
        std::fs::write(&original, "seed").expect("input");
        if hard_link {
            std::fs::hard_link(&original, &alias).expect("hard link");
        } else {
            std::os::unix::fs::symlink(&original, &alias).expect("symbolic link");
        }
        let main = workflow(
            &format!(
                "  first: {{run: writer.cwl, in: {{f: {{source: data, valueFrom: \
                 '${{return {{class: \"File\", location: self.location.replace(\"data.txt\", \"alias.txt\")}};}}'}}}}, out: []}}\n  \
                 read: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n"
            ),
            false,
        );
        let (mut driver, writer) = start(root.path(), &main, false, false);
        assert_eq!(writer.len(), 1);
        let mut outcome = written(root.path(), &writer[0], "new version");
        outcome.files[0].path = "alias.txt".into();
        let readers = driver.complete(outcome).expect("writer");
        assert_eq!(readers.len(), 1);
        assert_eq!(readers[0].command, ["read", "new version"]);
        assert_eq!(std::fs::read_to_string(original).expect("input"), "seed");
    }
}

#[test]
fn writers_wait_readers() {
    for reverse in [false, true] {
        let root = tempfile::tempdir().expect("folder");
        let main = workflow(
            &format!(
                "  one: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n  \
             two: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n  \
             write: {{run: writer.cwl, in: {{f: data}}, out: []}}\n"
            ),
            false,
        );
        let (mut driver, mut readers) = start(root.path(), &main, false, false);
        assert_eq!(readers.len(), 2);
        assert!(readers.iter().all(|task| task.command == ["read", "seed"]));
        if reverse {
            readers.reverse();
        }
        assert!(
            driver
                .complete(Outcome {
                    id: readers[0].id,
                    ..Outcome::default()
                })
                .expect("reader")
                .is_empty()
        );
        let writer = driver
            .complete(Outcome {
                id: readers[1].id,
                ..Outcome::default()
            })
            .expect("reader");
        assert_eq!(writer.len(), 1);
        assert_eq!(writer[0].command[0], "update");
        assert!(
            driver
                .complete(written(root.path(), &writer[0], "changed"))
                .expect("writer")
                .is_empty()
        );
        assert!(driver.outputs().is_some());
    }
}

#[test]
fn writers_share_inputs() {
    for scattered in [false, true] {
        let root = tempfile::tempdir().expect("folder");
        let main = if scattered {
            workflow(
                "  write: {run: writer.cwl, in: {f: data}, out: [], scatter: f}\n",
                true,
            )
        } else {
            let nested = workflow(
                "  first: {run: writer.cwl, in: {f: data}, out: []}\n  \
                second: {run: writer.cwl, in: {f: data}, out: []}\n",
                false,
            );
            std::fs::write(root.path().join("nested.cwl"), nested).expect("nested workflow");
            workflow(
                "  nested: {run: nested.cwl, in: {data: data}, out: []}\n",
                false,
            )
        };
        let (mut driver, first) = start(root.path(), &main, scattered, false);
        assert_eq!(first.len(), 1);
        let second = driver
            .complete(written(root.path(), &first[0], "new bytes"))
            .expect("first writer");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].command, ["update", "new bytes", "9"]);
        assert!(
            driver
                .complete(written(root.path(), &second[0], "last bytes"))
                .expect("second writer")
                .is_empty()
        );
        assert!(
            driver.outputs().is_some(),
            "nested/scatter bookkeeping must finish"
        );
    }
}

#[test]
fn failure_releases_work() {
    let root = tempfile::tempdir().expect("folder");
    let main = workflow(
        &format!(
            "  first: {{run: writer.cwl, in: {{f: data}}, out: []}}\n  \
         read: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n"
        ),
        false,
    );
    let (mut driver, writer) = start(root.path(), &main, false, true);
    assert_eq!(writer.len(), 1);
    let readers = driver
        .complete(Outcome {
            id: writer[0].id,
            exit_code: 1,
            ..Outcome::default()
        })
        .expect("records writer failure");
    assert_eq!(readers.len(), 1);
    assert_eq!(readers[0].command, ["read", "seed"]);
    driver
        .complete(Outcome {
            id: readers[0].id,
            ..Outcome::default()
        })
        .expect("independent reader");
    assert!(
        driver
            .finish()
            .expect_err("writer failure retained")
            .message
            .contains("exit code 1")
    );
}

#[test]
fn preparation_releases_readers() {
    for keep_going in [false, true] {
        for writer_fails in [false, true] {
            let root = tempfile::tempdir().expect("folder");
            std::fs::write(
                root.path().join("bad.cwl"),
                READER.replace("outputs: []", "outputs: {out: string}"),
            )
            .expect("bad reader tool");
            let main = workflow(
                &format!(
                    "  a_write: {{run: writer.cwl, in: {{f: data}}, out: []}}\n  \
                 b_bad: {{run: bad.cwl, in: {{content: {{source: data, loadContents: true, \
                 valueFrom: '${{throw new Error(\"bad reader\");}}'}}}}, out: [out]}}\n  \
                 c_read: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n  \
                 d_dependent: {{run: reader.cwl, in: {{content: b_bad/out}}, out: []}}\n"
                ),
                false,
            );
            let (mut driver, writer) = start(root.path(), &main, false, keep_going);
            assert_eq!(writer.len(), 1);
            let outcome = if writer_fails {
                Outcome {
                    id: writer[0].id,
                    exit_code: 1,
                    ..Outcome::default()
                }
            } else {
                written(root.path(), &writer[0], "updated")
            };
            let readers = driver
                .complete(outcome)
                .expect("failure goes through driver policy");
            if keep_going {
                assert_eq!(
                    readers.len(),
                    1,
                    "later independent reader must be admitted"
                );
                assert_eq!(
                    readers[0].command,
                    ["read", if writer_fails { "seed" } else { "updated" }]
                );
                assert!(
                    driver
                        .complete(Outcome {
                            id: readers[0].id,
                            ..Outcome::default()
                        })
                        .expect("independent reader completes")
                        .is_empty(),
                    "failed dependency must remain unavailable"
                );
            } else {
                assert!(
                    readers.is_empty(),
                    "stop policy must not execute queued readers"
                );
                assert!(driver.draining());
            }
            assert!(driver.outputs().is_none());
            let error = driver
                .finish()
                .expect_err("preparation failure retained")
                .message;
            if keep_going || !writer_fails {
                assert!(error.contains("bad reader"), "{error}");
            } else {
                assert!(
                    !error.contains("bad reader"),
                    "stop policy must not evaluate the bad reader: {error}"
                );
            }
            if writer_fails {
                assert!(
                    error.starts_with("the command ended with exit code 1"),
                    "primary writer failure: {error}"
                );
            }
        }
    }
}

#[test]
fn stop_prevents_preparation() {
    let root = tempfile::tempdir().expect("folder");
    let bad = WRITER.replace(
        "arguments: [$(inputs.f.contents), $(inputs.f.size)]",
        "arguments: ['${throw new Error(\"writer preparation resumed after stop\");}']",
    );
    std::fs::write(root.path().join("badwriter.cwl"), bad).expect("deferred writer");
    let main = workflow(
        &format!(
            "  one: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n  \
         two: {{run: reader.cwl, in: {READ_INPUT}, out: []}}\n  \
         write: {{run: badwriter.cwl, in: {{f: data}}, out: []}}\n"
        ),
        false,
    );
    let (mut driver, readers) = start(root.path(), &main, false, false);
    assert_eq!(readers.len(), 2);
    assert!(
        driver
            .complete(Outcome {
                id: readers[0].id,
                exit_code: 1,
                ..Outcome::default()
            })
            .expect("first reader fails")
            .is_empty()
    );
    assert!(
        driver
            .complete(Outcome {
                id: readers[1].id,
                ..Outcome::default()
            })
            .expect("other reader completes")
            .is_empty()
    );
    assert_eq!(
        driver.finish().expect_err("original failure only").message,
        "the command ended with exit code 1"
    );
}
