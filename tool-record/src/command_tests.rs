//! Tests of the command line parser with fixed command lines.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

use super::*;

fn parsed(line: &str) -> Tool {
    let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    parse(&words, Path::new("/nonexistent"))
}

#[test]
fn finds_base_commands() {
    let cases: [(&str, &[&str]); 7] = [
        ("python script.py --arg1 hello", &["python", "script.py"]),
        ("echo hello", &["echo"]),
        ("Rscript plot.R", &["Rscript", "plot.R"]),
        ("python", &["python"]),
        ("Rake build", &["Rake"]),
        ("python3.11 script.py", &["python3.11", "script.py"]),
        ("python3 -m my_module", &["python3", "-m", "my_module"]),
    ];
    for (line, expected) in cases {
        assert_eq!(parsed(line).base_command, expected, "{line}");
    }
}

#[test]
fn names_by_script() {
    let name_of = |line: &str| name(&parsed(line).base_command);
    assert_eq!(name_of("python scripts/plot.py data.csv"), "plot");
    assert_eq!(name_of("python3 -m my_module"), "my_module");
    assert_eq!(name_of("cat input.txt"), "cat");
}

#[test]
fn reads_option_kinds() {
    let tool = parsed("tool --argument1 value1 --flag -a value2 positional1 -v 1 --threshold -5");
    let summary: Vec<(&str, Kind, Option<Json>, &Binding)> = (tool.inputs.iter())
        .map(|input| {
            (
                input.id.as_str(),
                input.kind,
                input.default.clone(),
                &input.binding,
            )
        })
        .collect();
    let prefix = |flag: &str| Binding::Prefix(flag.to_owned());
    assert_eq!(
        summary,
        [
            (
                "argument1",
                Kind::String,
                Some(json!("value1")),
                &prefix("--argument1")
            ),
            ("flag", Kind::Boolean, Some(json!(true)), &prefix("--flag")),
            ("a", Kind::String, Some(json!("value2")), &prefix("-a")),
            (
                "positional1",
                Kind::String,
                Some(json!("positional1")),
                &Binding::Position(5)
            ),
            ("v", Kind::Int, Some(json!(1)), &prefix("-v")),
            (
                "threshold",
                Kind::Int,
                Some(json!(-5)),
                &prefix("--threshold")
            ),
        ]
    );
}

#[test]
fn keeps_word_boundaries() {
    let tool = parsed("tool --dry-run --max-retries 3");
    let ids: Vec<&str> = tool.inputs.iter().map(|input| input.id.as_str()).collect();
    assert_eq!(ids, ["dry_run", "max_retries"]);
}

#[test]
fn reads_type_hints() {
    let tool = parsed("tool f:later.csv i:7 s:12");
    let kinds: Vec<Kind> = tool.inputs.iter().map(|input| input.kind).collect();
    assert_eq!(kinds, [Kind::File, Kind::Int, Kind::String]);
    assert_eq!(tool.inputs[2].default, Some(json!("12")));
}

#[test]
fn reads_redirections() {
    let tool = parsed("cat input.txt > output.txt 2> errors.txt");
    assert_eq!(tool.stdout.as_deref(), Some("output.txt"));
    assert_eq!(tool.stderr.as_deref(), Some("errors.txt"));
    assert_eq!(tool.inputs.len(), 1);
    assert!(parsed("echo hello >").stdout.is_none());
}

#[test]
fn keeps_piped_words() {
    let tool = parsed("df | grep --line-buffered tmpfs > df.log");
    assert!(
        tool.stdout.is_none(),
        "the redirection belongs to the piped command"
    );
    let words: Vec<(&str, bool)> = (tool.arguments.iter())
        .map(|argument| (argument.value.as_str(), argument.quoted))
        .collect();
    assert_eq!(
        words,
        [
            ("|", false),
            ("grep", true),
            ("--line-buffered", true),
            ("tmpfs", true),
            (">", false),
            ("df.log", true)
        ]
    );
}

#[test]
fn hides_secret_values() {
    let tool = parsed("pg_dump postgres://postgres:password@localhost:5432/test > dump.sql");
    assert_eq!(tool.inputs[0].id, "secret_1");
    assert_eq!(tool.inputs[0].default, None);
}

#[test]
fn finds_local_paths() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("data.json"), "{}").unwrap();
    std::fs::write(folder.path().join("run.py"), "").unwrap();
    std::fs::create_dir(folder.path().join("reads")).unwrap();
    let words = [
        "python",
        "run.py",
        "--input",
        "data.json",
        "reads",
        "https://example.org/a",
    ];
    let words: Vec<String> = words.map(str::to_owned).to_vec();
    let tool = parse(&words, folder.path());
    let kinds: Vec<Kind> = tool.inputs.iter().map(|input| input.kind).collect();
    assert_eq!(kinds, [Kind::File, Kind::Directory, Kind::File]);
    assert_eq!(
        tool.inputs[1].default,
        Some(json!({ "class": "Directory", "location": "reads" }))
    );
    assert_eq!(tool.staged, ["run.py"]);
}
