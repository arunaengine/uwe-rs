//! The help of a command: its usage and the options that it takes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::options::{COMMANDS, OPTIONS, WORKFLOW};

pub fn help(command: &str) -> Result<String, String> {
    if !COMMANDS.contains(&command) {
        return Err(format!("unknown command `{command}`"));
    }
    let mut help = format!("usage: uwe {command} [options]");
    let target = match command {
        "runs" | "presets" | "clean" | "check-machine" | "version" | "serve" | "tool list" => "",
        "init" => " [folder]",
        "completions" => " <bash|zsh|fish>",
        "runs show" | "runs cancel" => " <run>",
        "runs logs" => " <run> [task]",
        "tool inspect" => " <id> [version]",
        "tool extract" => " <tool file>",
        "tool check" => " <composed workflow>",
        "tool suggest" => " <words>",
        "page" => " [run]",
        "compare" => " <run> [run]",
        "why" => " [run [task]]",
        "resume" => " <run> [step] [name=value ...]",
        "replay" => " <run> <task>",
        "config" => " [project] [--param value]",
        "origin" => " <file>",
        "digest" => " <path>",
        "record" => " -- <command ...>",
        "lower" => " <workflow.cwl|workflow.wdl|Snakefile|pipeline> [--param value]",
        _ if WORKFLOW.contains(&command) => " <workflow> [targets] [--param value]",
        "lock-environment" => " <environment.yml>",
        _ => " <run>",
    };
    help.push_str(target);
    if command == "run" {
        help.push_str("\n\nDefault executor: local (Docker or Podman containers). Use -executor simulator for made-up outputs.");
    }
    if WORKFLOW.contains(&command) {
        help.push_str("\n\nOptions take `-name` or, before `--`, `--name`; after `--` every `--name value` is a workflow parameter.");
    }
    help.push_str("\n\noptions:\n");
    for (names, commands) in OPTIONS {
        if commands.contains(&command) {
            let (flags, value) = names.split_at(names.find(' ').unwrap_or(names.len()));
            help.push_str(&format!("  {}{value}\n", flags.replace(',', " | ")));
        }
    }
    Ok(help)
}
