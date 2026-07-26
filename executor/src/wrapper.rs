//! The Bash wrapper that runs a task in its work directory and collects its outputs
//! (design section 5.4). It only knows the neutral names of the specification.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use task_model::{EnvironmentValue, TaskSpec};

use crate::braces::expand_braces;

/// The names of the control files the wrapper writes, and of the folder that holds copies of
/// outputs at absolute paths.
pub const EXIT: &str = "exit";
pub const START: &str = "start";
pub const END: &str = "end";
pub const MANIFEST: &str = "manifest";
pub const STDOUT: &str = "stdout";
pub const STDERR: &str = "stderr";
pub const STDIN: &str = "stdin";
pub const ROOT: &str = "root";
/// Writes the time in seconds since the Unix epoch: with microseconds from Bash 5 on, whose
/// `EPOCHREALTIME` needs no program, else whole seconds from `date`.
const NOW: &str = "echo \"${EPOCHREALTIME:-$(date +%s)}\"";
/// What the task used, as the cgroup of its container counts it: `cpu_usec`, `memory_peak`,
/// `read_bytes` and `write_bytes`, one per line with its number.
pub const USAGE: &str = "usage";

/// The wrapper for a task whose work and control directories are at the given absolute paths
/// inside the container. The task runs the arguments of the specification, which must not be
/// empty.
///
/// It runs the arguments with both streams captured in the control directory, so that output
/// patterns cannot match the capture files, and with `stdin` of the control directory as
/// standard input when the task has one. It records the exit code and times, collects the
/// outputs only on success, writes the manifest, and exits with 0 whenever it got that far, so
/// that the logs of a failed tool are kept. Outputs at absolute paths are copied below `root`
/// in the control directory, so that they outlive the container.
pub fn wrapper(spec: &TaskSpec, work: &str, control: &str) -> String {
    wrapper_with_outbox(spec, work, control, "")
}

/// The wrapper, which also copies each output in the work directory to the same path in the
/// `outbox` directory, for servers that give back only the directories a task names, like TES.
/// An empty `outbox` copies nothing. With an outbox, the wrapper at last lets every user change
/// what the task made, since such a server may run the container as root and then cannot remove
/// the folders that the task made.
pub fn wrapper_with_outbox(spec: &TaskSpec, work: &str, control: &str, outbox: &str) -> String {
    let mut script = String::from("#!/bin/bash\n# The iunctus task wrapper.\nset -u\n");
    script.push_str(&format!(
        "work={}\ncontrol={}\noutbox={}\n",
        quote(work),
        quote(control),
        quote(outbox)
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
    let stdin = match spec.stdin {
        Some(_) => format!(" < \"$control/{STDIN}\""),
        None => String::new(),
    };
    script.push_str(&format!("{NOW} > \"$control/{START}\"\n"));
    script.push_str(&format!(
        "{} > \"$control/{STDOUT}\" 2> \"$control/{STDERR}\"{stdin}\n",
        arguments.join(" ")
    ));
    script.push_str(&format!(
        "status=$?\n{NOW} > \"$control/{END}\"\necho \"$status\" > \"$control/{EXIT}\"\n"
    ));
    script.push_str(USAGE_SCRIPT);
    script.push_str(COLLECT);
    script.push_str(&format!(": > \"$control/{MANIFEST}\"\n"));
    // Bash rejects an empty `then` block, so a task without outputs gets none.
    if !spec.outputs.is_empty() {
        script.push_str("if [ \"$status\" -eq 0 ]; then\n");
        for rule in &spec.outputs {
            for pattern in expand_braces(rule.pattern.trim_end_matches('/')) {
                script.push_str(&format!("    collect {}\n", quote(&pattern)));
            }
        }
        script.push_str("fi\n");
    }
    if !outbox.is_empty() {
        script.push_str("chmod -R a+rwX \"$work\" \"$outbox\" \"$control\" 2> /dev/null\n");
    }
    script.push_str("exit 0\n");
    script
}

/// Records what the task used, from the cgroup of the container, when the container has one of
/// its own, as with Docker and Podman: CPU time, the peak of memory (page cache included) and
/// the bytes of block I/O. Bash builtins only, so that any image with Bash has what it needs.
const USAGE_SCRIPT: &str = r#"cgroup=/sys/fs/cgroup
if [ -r "$cgroup/memory.peak" ]; then
    {
        echo "memory_peak $(< "$cgroup/memory.peak")"
        if [ -r "$cgroup/cpu.stat" ]; then
            while read -r key value; do
                [ "$key" = usage_usec ] && echo "cpu_usec $value"
            done < "$cgroup/cpu.stat"
        fi
        if [ -r "$cgroup/io.stat" ]; then
            read_bytes=0 write_bytes=0
            while read -r -a words; do
                for word in "${words[@]}"; do
                    case $word in
                        rbytes=*) read_bytes=$((read_bytes + ${word#rbytes=})) ;;
                        wbytes=*) write_bytes=$((write_bytes + ${word#wbytes=})) ;;
                    esac
                done
            done < "$cgroup/io.stat"
            echo "read_bytes $read_bytes"
            echo "write_bytes $write_bytes"
        fi
    } > "$control/usage" 2> /dev/null
fi
"#;

/// Adds each file and directory that a pattern matches to the manifest, as
/// `file <size> <path>` or `directory <path>`. Bash expands the pattern with `globstar`,
/// `nullglob` and `dotglob`. A match at an absolute path is copied below `root` in the control
/// directory, and one in the work directory to the outbox, when there is one; the copies hold
/// what links point to.
const COLLECT: &str = r#"shopt -s globstar nullglob dotglob
collect() {
    local IFS=$'\n' match
    for match in $1; do
        match=${match#./}
        case $match in
            /*) [ -e "$control/root$match" ] || {
                    mkdir -p "$control/root${match%/*}" && cp -a "$match" "$control/root$match"
                } ;;
            *) [ -z "$outbox" ] || [ -e "$outbox/$match" ] || {
                    mkdir -p "$outbox/$(dirname "$match")" && cp -RL "$match" "$outbox/$match"
                } ;;
        esac
        if [ -d "$match" ]; then
            printf 'directory %s\n' "$match" >> "$control/manifest"
        elif [ -f "$match" ]; then
            printf 'file %s %s\n' "$(wc -c < "$match")" "$match" >> "$control/manifest"
        fi
    done
}
"#;

/// A Bash word in single quotes.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}
