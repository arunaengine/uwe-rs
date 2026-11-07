//! Runs WDL workflows with the uwe binary in Docker: the hello demo, and a set of examples
//! of the WDL 1.2 specification with their inputs and expected outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value;

mod common;

/// Runs uwe in a folder with its own cache; gives the exit code, stdout and both streams.
fn uwe(directory: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs uwe");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let both = format!("{stdout}{stderr}");
    (output.status.code().unwrap_or(-1), stdout, both)
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_hello_demo() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../demo/wdl/hello.wdl");
    std::fs::copy(demo, root.path().join("hello.wdl")).expect("copies the demo");
    let arguments = [
        "run",
        "hello.wdl",
        "--greeting",
        "Hola",
        "-executor",
        "local",
    ];
    let (code, _, output) = uwe(root.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] say_hello"), "{output}");
    assert!(
        output.contains("\"hello.message\": \"Hola, world\""),
        "{output}"
    );
    let delivered = root.path().join("greeting_file/greeting.txt");
    assert_eq!(
        std::fs::read_to_string(delivered).expect("a delivered file"),
        "Hola, world\n"
    );
}

/// Examples of the WDL specification that the first WDL subset of uwe covers, with whether
/// each passes. The specification is that of tag v1.2.1, commit
/// 42c106d0b12b4af06d619262a99e94aef028f7fe of github.com/openwdl/wdl.
const EXAMPLES: &[(&str, bool)] = &[
    ("sum_task", true),
    ("task_outputs", true),
    ("declarations", true),
    ("expressions_task", true),
    ("ternary", true),
    ("placeholders", true),
    ("flags_task", true),
    ("default_option_task", true),
    ("private_declaration_task", true),
    ("outputs_task", true),
    ("file_output_task", true),
    ("glob_task", true),
    ("optional_output_task", true),
    ("test_containers", true),
    ("test_cpu_task", true),
    // Its command runs `free`, which the default image debian:stable-slim does not have.
    ("test_memory_task", false),
    ("single_return_code_task", true),
    ("all_return_codes_task", true),
    ("call_example", true),
    ("test_scatter", true),
    ("nested_scatter", true),
    ("test_conditional", true),
    ("nested_if", true),
    ("test_floor", true),
    ("test_sub", true),
    ("test_basename", true),
    ("echo_stdout_task", true),
    ("read_int_task", true),
    ("write_lines_task", true),
    ("test_zip", true),
    ("test_select_first", true),
    ("test_collect_by_key", true),
    ("test_length", true),
    ("environment_variable_should_echo", true),
];

#[test]
#[ignore = "needs Docker, the WDL specification at UWE_WDL_SPEC and its test data at UWE_WDL_DATA"]
fn runs_spec_examples() {
    let spec =
        std::env::var("UWE_WDL_SPEC").expect("UWE_WDL_SPEC names SPEC.md of the WDL 1.2.1 tag");
    let data = std::env::var("UWE_WDL_DATA").ok();
    let text = std::fs::read_to_string(&spec).expect("reads the specification");
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut differences = Vec::new();
    let mut passed = 0;
    for (name, expected) in EXAMPLES {
        let example = Example::find(&text, name).unwrap_or_else(|| panic!("no example {name}"));
        let outcome = example.run(root.path(), data.as_deref().map(Path::new));
        println!(
            "{name}: {}",
            outcome.as_ref().err().map_or("pass", String::as_str)
        );
        passed += usize::from(outcome.is_ok());
        if outcome.is_ok() != *expected {
            differences.push(format!("{name}: {outcome:?}"));
        }
    }
    println!("{passed} of {} examples pass", EXAMPLES.len());
    assert!(differences.is_empty(), "outcomes changed: {differences:#?}");
}

/// One example of the specification: its document, inputs, outputs and test settings.
struct Example {
    name: String,
    document: String,
    inputs: Value,
    outputs: Value,
    config: Value,
    /// The examples that the document imports, like `other.wdl`, and their imports.
    imports: Vec<Example>,
}

impl Example {
    fn find(text: &str, name: &str) -> Option<Example> {
        let start = text.find(&format!("Example: {name}.wdl\n"))?;
        let rest = &text[start..];
        let end = rest.find("</details>").unwrap_or(rest.len());
        let rest = &rest[..end];
        let block = |label: &str, fence: &str| -> Option<String> {
            let at = rest.find(label)?;
            let body = &rest[at..];
            let open = body.find(fence)? + fence.len();
            let close = body[open..].find("```")?;
            Some(body[open..open + close].to_owned())
        };
        let json = |label: &str| {
            (block(label, "```json\n"))
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Value::Null)
        };
        let document = block("Example: ", "```wdl\n")?;
        let imports = (document.lines())
            .filter_map(|line| line.trim().strip_prefix("import \""))
            .filter_map(|line| line.split_once(".wdl\""))
            .filter_map(|(imported, _)| Example::find(text, imported))
            .flat_map(|mut example| {
                let mut all = std::mem::take(&mut example.imports);
                all.push(example);
                all
            })
            .collect();
        Some(Example {
            name: name.to_owned(),
            document,
            imports,
            inputs: json("Example input:"),
            outputs: json("Example output:"),
            config: json("Test config:"),
        })
    }

    /// The workflow or task to run: the name in the keys of the inputs or outputs.
    fn target(&self) -> Option<String> {
        let keys = |value: &Value| {
            value
                .as_object()
                .and_then(|fields| fields.keys().next().cloned())
        };
        let key = keys(&self.outputs).or_else(|| keys(&self.inputs))?;
        key.split('.').next().map(str::to_owned)
    }

    fn run(&self, root: &Path, data: Option<&Path>) -> Result<(), String> {
        let folder = root.join(&self.name);
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        if let Some(data) = data {
            let target = folder.join("data");
            std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
            for entry in std::fs::read_dir(data)
                .map_err(|error| error.to_string())?
                .flatten()
            {
                std::fs::copy(entry.path(), target.join(entry.file_name()))
                    .map_err(|error| error.to_string())?;
            }
        }
        let file = format!("{}.wdl", self.name);
        std::fs::write(folder.join(&file), &self.document).map_err(|error| error.to_string())?;
        for imported in &self.imports {
            std::fs::write(
                folder.join(format!("{}.wdl", imported.name)),
                &imported.document,
            )
            .map_err(|error| error.to_string())?;
        }
        let inputs = match &self.inputs {
            Value::Null => serde_json::json!({}),
            inputs => inputs.clone(),
        };
        std::fs::write(folder.join("inputs.json"), inputs.to_string())
            .map_err(|error| error.to_string())?;
        let mut arguments = vec![
            "run",
            &file,
            "inputs.json",
            "-executor",
            "local",
            "-output-dir",
            "out",
        ];
        let target = self.target();
        if let Some(target) = &target {
            arguments.extend(["-entry", target.as_str()]);
        }
        let (code, stdout, output) = uwe(&folder, &arguments);
        let fails = self.config["fail"].as_bool().unwrap_or(false);
        match (fails, code) {
            (true, 0) => return Err("the run should fail".into()),
            (true, _) => return Ok(()),
            (false, 0) => {}
            (false, _) => return Err(last_line(&output)),
        }
        let actual = printed(&stdout).ok_or("no outputs were printed")?;
        let excluded: Vec<&str> = (self.config["exclude_outputs"]
            .as_array()
            .into_iter()
            .flatten())
        .filter_map(Value::as_str)
        .collect();
        let expected = self.outputs.as_object().cloned().unwrap_or_default();
        for (key, value) in expected {
            let short = key.split_once('.').map_or(key.as_str(), |(_, short)| short);
            if excluded.contains(&key.as_str()) || excluded.contains(&short) {
                continue;
            }
            let found = actual.get(&key).ok_or(format!("no output {key}"))?;
            if !same(&value, found) {
                return Err(format!("{key} is {found}, not {value}"));
            }
        }
        Ok(())
    }
}

/// The outputs object that uwe printed: the lines from a `{` to a `}` in the first column.
fn printed(stdout: &str) -> Option<serde_json::Map<String, Value>> {
    if stdout.lines().any(|line| line == "{}") {
        return Some(serde_json::Map::new());
    }
    let start = stdout
        .find("\n{\n")
        .map(|at| at + 1)
        .or(stdout.starts_with("{\n").then_some(0))?;
    let end = stdout[start..].find("\n}\n")? + start + 2;
    serde_json::from_str(&stdout[start..end]).ok()
}

/// Whether an output is as the specification expects: files by their names, numbers by value.
fn same(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::String(name), Value::String(path)) if path.starts_with('/') => {
            path.rsplit('/').next() == name.rsplit('/').next()
        }
        (Value::Number(a), Value::Number(b)) => {
            (a.as_f64().unwrap_or(0.0) - b.as_f64().unwrap_or(0.0)).abs() < 1e-6
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
        }
        _ => expected == actual,
    }
}

fn last_line(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| line.contains("error") || line.starts_with("uwe:"))
        .collect();
    lines.last().map_or_else(
        || output.lines().last().unwrap_or_default().to_owned(),
        |line| (*line).to_owned(),
    )
}
