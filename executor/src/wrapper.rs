//! The Bash wrapper that runs a task in its work directory and collects its outputs
//! (design section 5.4). It only knows the neutral names of the specification.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use task_model::{EnvironmentValue, TaskSpec};

/// The names of the control files the wrapper writes.
pub const EXIT: &str = "exit";
pub const START: &str = "start";
pub const END: &str = "end";
pub const MANIFEST: &str = "manifest";

/// The wrapper for a task whose work and control directories are at the given absolute paths.
///
/// It runs the arguments with both streams redirected, records the exit code and times,
/// collects the outputs only on success, writes the manifest, and exits with 0 whenever it
/// got that far, so that the logs of a failed tool are kept.
pub fn wrapper(spec: &TaskSpec, work: &str, control: &str) -> String {
    let mut script = String::from("#!/bin/bash\n# The iunctus task wrapper.\nset -u\n");
    script.push_str(&format!(
        "work={}\ncontrol={}\n",
        quote(work),
        quote(control)
    ));
    script.push_str("mkdir -p \"$work\" \"$control\"\ncd \"$work\" || exit 1\n");
    for (name, value) in &spec.environment {
        let value = match value {
            EnvironmentValue::Literal(text) => quote(text),
            EnvironmentValue::WorkDirectory => "\"$work\"".to_owned(),
        };
        script.push_str(&format!("export {name}={value}\n"));
    }
    let arguments: Vec<String> = spec
        .arguments
        .iter()
        .map(|argument| quote(argument))
        .collect();
    script.push_str(&format!("date +%s > \"$control/{START}\"\n"));
    script.push_str(&format!(
        "{} > {} 2> {}\n",
        arguments.join(" "),
        quote(&spec.stdout),
        quote(&spec.stderr)
    ));
    script.push_str(&format!(
        "status=$?\necho \"$status\" > \"$control/{EXIT}\"\n"
    ));
    script.push_str(&format!("date +%s > \"$control/{END}\"\n"));
    script.push_str(COLLECT);
    script.push_str(&format!(": > \"$control/{MANIFEST}\"\n"));
    script.push_str(&format!(
        "collect {}\ncollect {}\n",
        quote(&spec.stdout),
        quote(&spec.stderr)
    ));
    script.push_str("if [ \"$status\" -eq 0 ]; then\n");
    for rule in &spec.outputs {
        script.push_str(&format!("    collect {}\n", quote(&rule.pattern)));
    }
    script.push_str("fi\nexit 0\n");
    script
}

/// Adds each file that a pattern matches to the manifest as `<size> <path>`; a directory adds
/// every file below it. Bash expands the pattern with `globstar`, `nullglob` and `dotglob`.
const COLLECT: &str = r#"shopt -s globstar nullglob dotglob
collect() {
    local IFS=$'\n' match file
    for match in $1; do
        if [ -d "$match" ]; then
            while IFS= read -r file; do
                printf '%s %s\n' "$(wc -c < "$file")" "${file#./}" >> "$control/manifest"
            done < <(find "$match" -type f)
        elif [ -f "$match" ]; then
            printf '%s %s\n' "$(wc -c < "$match")" "${match#./}" >> "$control/manifest"
        fi
    done
}
"#;

/// A Bash word in single quotes.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}
