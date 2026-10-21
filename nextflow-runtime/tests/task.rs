//! Checks the container task of a Nextflow task, its outcome, and the capture of `env` and
//! `eval` outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::process::Command;
use std::rc::Rc;

use nextflow_evaluator::interpreter::TaskDirective;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{task, task_outcome};
use nextflow_runtime::task::{command_script, read_environment};
use nextflow_runtime::{TaskId, TaskRequest};
use workflow_graph::{Outcome, OutputFile};

fn request(eval: &[&str], env: &[&str], container: Option<&str>) -> TaskRequest {
    let mut directives = vec![TaskDirective {
        name: "cpus".into(),
        arguments: vec![Value::integer(2)],
    }];
    if let Some(image) = container {
        directives.push(TaskDirective {
            name: "container".into(),
            arguments: vec![Value::string(image)],
        });
    }
    TaskRequest {
        id: TaskId(1),
        attempt: 1,
        process: "P".into(),
        name: "P (1)".into(),
        script: "#!/bin/bash -ue\nFOO='a b'\necho hi\n".into(),
        directives,
        staged: vec![(PathValue::parse("/data/x.gz", "/"), "x.gz".into())],
        input_environment: Vec::new(),
        stdin: None,
        output_patterns: vec!["out.txt".into()],
        env: env.iter().map(|name| Rc::from(*name)).collect(),
        eval: eval.iter().map(|command| (*command).to_owned()).collect(),
        made: None,
    }
}

#[test]
fn captures_task_outputs() {
    let request = request(&["echo \"v'1\"", "echo bad >&2; exit 2"], &["FOO"], None);
    let script = command_script(&request);
    assert!(script.starts_with(&request.script));
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join(".command.sh"), &script).expect("writes the script");
    let status = Command::new("bash")
        .args(["-ue", ".command.sh"])
        .current_dir(root.path())
        .output()
        .expect("runs bash");
    assert!(status.status.success());
    let text =
        std::fs::read_to_string(root.path().join(".command.env")).expect("reads .command.env");
    let variables = read_environment(&text);
    assert_eq!(variables["FOO"], ("a b".to_owned(), 0));
    assert_eq!(variables["nxf_out_eval_1"], ("v'1".to_owned(), 0));
    assert_eq!(variables["nxf_out_eval_2"], ("bad".to_owned(), 2));
    assert!(!root.path().join(".command.eval").exists());
}

#[test]
fn preserves_uncaptured_scripts() {
    let request = request(&[], &[], None);
    assert_eq!(command_script(&request), request.script);
}

#[test]
fn disables_task_reuse() {
    let mut request = request(&[], &[], None);
    for (value, expected) in [
        (Value::Boolean(false), workflow_graph::Reuse::Disabled),
        (Value::Boolean(true), workflow_graph::Reuse::Allowed),
        (Value::string("deep"), workflow_graph::Reuse::Allowed),
    ] {
        request.directives.push(TaskDirective {
            name: "cache".into(),
            arguments: vec![value],
        });
        assert_eq!(task(&request, &BTreeMap::new()).unwrap().reuse, expected);
    }
}

#[test]
fn forces_fresh_retries() {
    let mut request = request(&[], &[], None);
    request.attempt = 2;
    for made in [None, Some(Vec::new())] {
        request.made = made;
        assert_eq!(
            task(&request, &BTreeMap::new()).unwrap().reuse,
            workflow_graph::Reuse::Denied
        );
        request.directives.push(TaskDirective {
            name: "cache".into(),
            arguments: vec![Value::Boolean(false)],
        });
        assert_eq!(
            task(&request, &BTreeMap::new()).unwrap().reuse,
            workflow_graph::Reuse::Disabled
        );
        request.directives.pop();
    }
}

#[test]
fn forwards_container_inputs() {
    let mut request = request(&[], &[], None);
    request.input_environment = vec![("SAMPLE".into(), Value::string("round one"))];
    request.stdin = Some(Value::string("first\nsecond\n"));
    let environment = BTreeMap::from([
        ("SAMPLE".into(), "config".into()),
        ("CONFIG_ONLY".into(), "retained".into()),
    ]);
    let first = task(&request, &environment).unwrap();
    assert_eq!(first.environment["SAMPLE"], "round one");
    assert_eq!(first.environment["CONFIG_ONLY"], "retained");
    assert_eq!(first.stdin.as_deref(), Some("first\nsecond\n"));
    request.input_environment[0].1 = Value::string("round two");
    request.stdin = Some(Value::string("different\n"));
    let second = task(&request, &environment).unwrap();
    assert_ne!(first.environment, second.environment);
    assert_ne!(first.stdin, second.stdin);
}

#[test]
fn container_tasks() {
    let plain = request(&[], &[], None);
    let container = task(&plain, &BTreeMap::new()).unwrap();
    assert_eq!(container.image, None);
    assert_eq!(container.step, "P");
    assert_eq!(container.command, ["/bin/bash", "-ue", ".command.sh"]);
    assert_eq!(container.script.as_deref(), Some(plain.script.as_str()));
    let paths: Vec<&str> = container
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    assert_eq!(paths, [".command.sh", "x.gz"]);
    assert_eq!(
        container.inputs[0].content.as_deref(),
        Some(plain.script.as_str())
    );
    assert_eq!(container.inputs[1].source, "/data/x.gz");
    assert_eq!(container.outputs, ["out.txt"]);
    assert_eq!(container.resources.cpus, Some(2.0));
    let tagged = request(&["true"], &[], Some("debian:stable-slim"));
    let environment = BTreeMap::from([("A".to_owned(), "1".to_owned())]);
    let container = task(&tagged, &environment).unwrap();
    assert_eq!(container.image.as_deref(), Some("debian:stable-slim"));
    assert_eq!(container.outputs, ["out.txt", ".command.env"]);
    assert_eq!(container.environment, environment);
    let script = &container.inputs[0].content;
    assert!(
        script
            .as_deref()
            .is_some_and(|text| text.contains("nxf_out_eval_1"))
    );
}

#[test]
fn forwards_valid_resources() {
    let mut request = request(&[], &[], None);
    request.directives.extend([
        TaskDirective {
            name: "memory".into(),
            arguments: vec![Value::MemoryUnit(2 << 30)],
        },
        TaskDirective {
            name: "disk".into(),
            arguments: vec![Value::string("1.5 GB")],
        },
        TaskDirective {
            name: "time".into(),
            arguments: vec![Value::Duration(90_000)],
        },
        TaskDirective {
            name: "accelerator".into(),
            arguments: vec![Value::Null],
        },
    ]);
    let resources = task(&request, &BTreeMap::new()).unwrap().resources;
    assert_eq!(resources.cpus, Some(2.0));
    assert_eq!(resources.memory_bytes, Some(2 << 30));
    assert_eq!(resources.disk_bytes, Some(3 << 29));
    assert_eq!(resources.time_seconds, Some(90));
    assert_eq!(resources.gpus, None);
}

#[test]
fn rejects_invalid_resources() {
    for (name, value) in [
        ("cpus", "many"),
        ("memory", "lots"),
        ("disk", "plenty"),
        ("time", "forever"),
        ("accelerator", "all"),
    ] {
        let mut request = request(&[], &[], None);
        request.directives.push(TaskDirective {
            name: name.into(),
            arguments: vec![Value::string(value)],
        });
        let error = task(&request, &BTreeMap::new()).unwrap_err().message;
        assert!(error.contains("process `P`"), "{error}");
        assert!(
            error.contains(&format!("`{name}` directive value `{value}`")),
            "{error}"
        );
    }
}

#[test]
fn rejects_numeric_resources() {
    let too_large = Number::parse_integer("18446744073709551616").unwrap();
    for (name, value, detail) in [
        ("cpus", Value::integer(-1), "cannot be negative"),
        ("memory", Value::integer(-1), "cannot be negative"),
        ("disk", Value::integer(-1), "cannot be negative"),
        ("time", Value::integer(-1), "cannot be negative"),
        ("accelerator", Value::integer(-1), "cannot be negative"),
        (
            "cpus",
            Value::Number(Number::Long(i64::from(i32::MAX) + 1)),
            "signed 32-bit count",
        ),
        (
            "accelerator",
            Value::Number(Number::Long(i64::from(i32::MAX) + 1)),
            "signed 32-bit count",
        ),
        (
            "memory",
            Value::Number(too_large.clone()),
            "signed 64-bit range",
        ),
        (
            "time",
            Value::Number(Number::Double(f64::INFINITY)),
            "signed 64-bit range",
        ),
    ] {
        let mut request = request(&[], &[], None);
        request.directives.push(TaskDirective {
            name: name.into(),
            arguments: vec![value],
        });
        let error = task(&request, &BTreeMap::new()).unwrap_err().message;
        assert!(error.contains(&format!("`{name}` directive")), "{error}");
        assert!(error.contains(detail), "{error}");
    }
}

#[test]
fn converts_fractional_cpus() {
    for (text, value) in [
        ("0", Value::integer(0)),
        (
            "0.75",
            Value::Number(Number::parse_decimal("0.75").unwrap()),
        ),
        (
            "1.75",
            Value::Number(Number::parse_decimal("1.75").unwrap()),
        ),
    ] {
        let mut request = request(&[], &[], None);
        request.directives.push(TaskDirective {
            name: "cpus".into(),
            arguments: vec![value],
        });
        let resources = task(&request, &BTreeMap::new()).unwrap().resources;
        assert_eq!(resources.cpus, Some(1.0), "cpus {text}");
    }
}

#[test]
fn returns_captured_values() {
    let request = request(&["echo v"], &["FOO"], None);
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = |name: &str, text: &str| {
        let location = directory.path().join(name);
        std::fs::write(&location, text).expect("writes a file");
        OutputFile {
            path: name.into(),
            location: location.to_string_lossy().into_owned(),
            ..OutputFile::default()
        }
    };
    let files = vec![
        file("out.txt", ""),
        file(
            ".command.env",
            "FOO=a b\n/FOO/\nnxf_out_eval_1=1.0\n/nxf_out_eval_1/=exit:0\n",
        ),
    ];
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files,
        stdout: "said\n".into(),
        ..Outcome::default()
    };
    let result = task_outcome(&request, outcome, None).expect("an outcome");
    let names: Vec<&str> = result.files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["out.txt"]);
    assert_eq!(result.eval, ["1.0"]);
    assert_eq!(result.env["FOO"], "a b");
    assert_eq!(result.stdout, "said\n");
    // An absent real capture is a collection failure, not a captured empty value.
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        ..Outcome::default()
    };
    let result = task_outcome(&request, outcome, None).expect("an outcome");
    assert!(result.failure.is_some());
    assert!(result.eval.is_empty());
    assert_eq!(result.env["FOO"], "");
    let empty = file(".command.env", "nxf_out_eval_1=\n/nxf_out_eval_1/=exit:0\n");
    let result = task_outcome(
        &request,
        Outcome {
            files: vec![empty],
            ..Outcome::default()
        },
        None,
    )
    .unwrap();
    assert!(result.failure.is_none());
    assert_eq!(result.eval, [""]);
    let failed = file(
        ".command.env",
        "nxf_out_eval_1=bad\n/nxf_out_eval_1/=exit:2\n",
    );
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files: vec![failed],
        ..Outcome::default()
    };
    let result = task_outcome(&request, outcome, None).expect("an outcome for the process policy");
    let error = result.failure.expect("the eval command failed");
    assert!(error.to_string().contains("exit status 2: bad"), "{error}");
}

#[test]
fn environment_files() {
    let text = "\nA=one\n/A/\nB=line 1\nline 2\n/B/=exit:0\n";
    let variables = read_environment(text);
    assert_eq!(variables["A"], ("one".to_owned(), 0));
    assert_eq!(variables["B"], ("line 1\nline 2".to_owned(), 0));
}

/// A store elsewhere, whose `s3://store/` locations have their content in a local folder.
struct Elsewhere(std::path::PathBuf);

impl workflow_graph::Files for Elsewhere {
    fn local(&self, location: &str) -> std::io::Result<std::path::PathBuf> {
        let key = location
            .strip_prefix("s3://store/")
            .ok_or(std::io::ErrorKind::NotFound)?;
        Ok(self.0.join(key))
    }

    fn deliver(&self, _delivery: &workflow_graph::Delivery) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn reads_stored_values() {
    let request = request(&[], &["FOO"], None);
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("env"), "FOO=remote\n/FOO/\n").expect("writes");
    let outcome = Outcome {
        id: workflow_graph::TaskId(1),
        files: vec![OutputFile {
            path: ".command.env".into(),
            location: "s3://store/env".into(),
            ..OutputFile::default()
        }],
        ..Outcome::default()
    };
    let files = Elsewhere(directory.path().to_owned());
    let result = task_outcome(&request, outcome, Some(&files)).expect("an outcome");
    assert_eq!(result.env["FOO"], "remote");
}

#[test]
fn hooks_preserve_status() {
    for (body, after, expected) in [
        ("true", "false", 1),
        ("exit 7", "false", 7),
        ("exit 7", "true", 7),
        ("exit 7", "exit 0", 7),
    ] {
        let mut request = request(&[], &[], None);
        request.script = format!("test \"$BEFORE\" = yes\ntest -e x.gz\n{body}\n");
        request.directives.extend([
            TaskDirective {
                name: "beforeScript".into(),
                arguments: vec![Value::string("test ! -e x.gz; export BEFORE=yes")],
            },
            TaskDirective {
                name: "afterScript".into(),
                arguments: vec![Value::string(format!("echo after > after.txt; {after}"))],
            },
        ]);
        let task = task(&request, &BTreeMap::new()).unwrap();
        let root = tempfile::tempdir().unwrap();
        for input in &task.inputs {
            let path = root.path().join(&input.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, input.content.as_deref().unwrap_or("input")).unwrap();
        }
        let output = Command::new(&task.command[0])
            .args(&task.command[1..])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root.path().join("after.txt").exists());
    }
}

#[test]
fn before_failure_stops() {
    let mut request = request(&[], &[], None);
    request.directives.push(TaskDirective {
        name: "beforeScript".into(),
        arguments: vec![Value::string("exit 9")],
    });
    let script = nextflow_runtime::task::launcher(&request).unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("hooks"), script).unwrap();
    let status = Command::new("bash")
        .args(["hooks", "touch", "body"])
        .current_dir(root.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(9));
    assert!(!root.path().join("body").exists());
}

#[test]
fn records_secret_names() {
    let mut request = request(&[], &[], None);
    request.directives.push(TaskDirective {
        name: "secret".into(),
        arguments: vec![Value::string("TOKEN"), Value::string("KEY")],
    });
    let task = task(&request, &BTreeMap::new()).unwrap();
    assert_eq!(
        task.environment[nextflow_runtime::task::SECRET_NAMES],
        "[\"KEY\",\"TOKEN\"]"
    );
    assert!(!task.environment.contains_key("TOKEN"));
    assert!(!task.environment.contains_key("KEY"));
    let launcher = task
        .inputs
        .iter()
        .find(|input| input.path == ".command.hooks")
        .unwrap()
        .content
        .as_ref()
        .unwrap();
    assert!(launcher.contains("source /run/iunctus/secrets"));
    assert!(
        task.inputs
            .iter()
            .all(|input| input.path != "/run/iunctus/secrets")
    );
}

#[test]
fn maps_container_options() {
    let mut request = request(&[], &[], None);
    request.directives.push(TaskDirective {
        name: "containerOptions".into(),
        arguments: vec![Value::string(
            "-e ANSWER='a b' --env=EMPTY= --cpus=3.5 --gpus 2",
        )],
    });
    let mapped = task(&request, &BTreeMap::new()).unwrap();
    assert_eq!(mapped.environment["ANSWER"], "a b");
    assert_eq!(mapped.environment["EMPTY"], "");
    assert_eq!(mapped.resources.cpus, Some(3.5));
    assert_eq!(mapped.resources.gpus, Some(2));
    for flag in [
        "--privileged",
        "--volume /:/host",
        "--device /dev/null",
        "--env INHERITED",
        "--env 'BROKEN=value",
        "--gpus device=0",
        "--env TOKEN=$HOME",
    ] {
        request.directives.last_mut().unwrap().arguments = vec![Value::string(flag)];
        let error = task(&request, &BTreeMap::new()).unwrap_err();
        assert!(error.unsupported, "{flag}: {error}");
    }
}

#[test]
fn maps_shared_memory() {
    let mut request = request(&[], &[], None);
    request.directives.push(TaskDirective {
        name: "containerOptions".into(),
        arguments: vec![Value::string(
            "--shm-size 0.2g --memory-swap 512mb --gpus all -eESCAPED=\"a\\b\" -eLITERAL='$HOME'",
        )],
    });
    let task = task(&request, &BTreeMap::new()).unwrap();
    let flags: serde_json::Value =
        serde_json::from_str(&task.environment["UWE_CONTAINER_FLAGS"]).unwrap();
    assert_eq!(flags["shm"], 214748364);
    assert_eq!(flags["swap"], 512 << 20);
    assert_eq!(flags["gpus"], -1);
    assert_eq!(task.resources.gpus, None);
    assert_eq!(task.environment["ESCAPED"], "a\\b");
    assert_eq!(task.environment["LITERAL"], "$HOME");
}

#[test]
fn maps_work_environment() {
    let mut request = request(&[], &[], None);
    request.directives.push(TaskDirective {
        name: "containerOptions".into(),
        arguments: vec![Value::string("-e HOME=$PWD -e LITERAL='$PWD'")],
    });
    let mapped = task(&request, &BTreeMap::new()).unwrap();
    assert_eq!(mapped.environment["UWE_WORK_VARIABLES"], "[\"HOME\"]");
    assert_eq!(mapped.environment["LITERAL"], "$PWD");
    request.directives.last_mut().unwrap().arguments = vec![Value::map([(
        Value::string("shm-size"),
        Value::MemoryUnit(4 << 30),
    )])];
    let mapped = task(&request, &BTreeMap::new()).unwrap();
    let flags: serde_json::Value =
        serde_json::from_str(&mapped.environment["UWE_CONTAINER_FLAGS"]).unwrap();
    assert_eq!(flags["shm"], 4i64 << 30);
}
