//! `uwe completions`: the shell completion of the commands, subcommands and options of uwe
//! for bash, zsh and fish, made from the table of options of the parser.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use crate::Failure;
use crate::arguments::{Arguments, options_of, subcommands, typed_commands};

/// The groups of commands, whose second word is a subcommand.
const GROUPS: [&str; 3] = ["runs", "tool", "cache"];

/// `uwe completions bash|zsh|fish`: prints the completion script of the shell.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let usage = || Failure::usage("uwe: use `uwe completions bash|zsh|fish`");
    let shell = arguments
        .target
        .as_ref()
        .ok_or_else(usage)?
        .to_string_lossy();
    if !arguments.targets.is_empty() {
        return Err(usage());
    }
    let script = match shell.as_ref() {
        "bash" => bash(),
        "zsh" => zsh(),
        "fish" => fish(),
        _ => return Err(usage()),
    };
    print!("{script}");
    Ok(())
}

/// Every command line that has options of its own: a command, or a group and its subcommand,
/// with the command that gives the options.
fn lines() -> Vec<(String, &'static str)> {
    let mut found: Vec<(String, &str)> = Vec::new();
    for command in typed_commands() {
        if GROUPS.contains(&command) {
            for (name, flat) in subcommands(command) {
                found.push((format!("{command} {name}"), flat));
            }
        }
        let flat = match command {
            "doctor" => "check-machine",
            "tool" => "tool list",
            other => other,
        };
        if !options_of(flat).is_empty() {
            found.push((command.to_owned(), flat));
        }
    }
    found
}

/// The options of a command in both spellings, `-name` and `--name`.
fn spellings(command: &str) -> String {
    let options = options_of(command);
    let long = options.iter().map(|option| format!("-{option}"));
    let words: Vec<String> = options
        .iter()
        .map(|option| option.to_string())
        .chain(long)
        .collect();
    words.join(" ")
}

fn bash() -> String {
    let mut script = String::from(
        "# The bash completion of uwe, from `uwe completions bash`.\n_uwe() {\n  \
         local current=\"${COMP_WORDS[COMP_CWORD]}\" line options=\"\"\n  \
         if [ \"$COMP_CWORD\" -eq 1 ]; then\n",
    );
    let _ = writeln!(
        script,
        "    COMPREPLY=($(compgen -W \"{}\" -- \"$current\"))\n    return\n  fi",
        typed_commands().join(" ")
    );
    script.push_str("  line=\"${COMP_WORDS[1]}\"\n  case \"$line\" in\n");
    for group in GROUPS {
        let names: Vec<&str> = subcommands(group).iter().map(|(name, _)| *name).collect();
        let _ = writeln!(
            script,
            "    {group})\n      if [ \"$COMP_CWORD\" -eq 2 ]; then\n        \
             COMPREPLY=($(compgen -W \"{}\" -- \"$current\"))\n        return\n      fi\n      \
             line=\"$line ${{COMP_WORDS[2]}}\" ;;",
            names.join(" ")
        );
    }
    script.push_str("  esac\n  case \"$line\" in\n");
    for (line, command) in lines() {
        let _ = writeln!(
            script,
            "    \"{line}\") options=\"{}\" ;;",
            spellings(command)
        );
    }
    script.push_str(
        "  esac\n  if [[ \"$current\" == -* ]]; then\n    \
         COMPREPLY=($(compgen -W \"$options\" -- \"$current\"))\n  else\n    \
         COMPREPLY=($(compgen -f -- \"$current\"))\n  fi\n}\ncomplete -o filenames -F _uwe uwe\n",
    );
    script
}

fn zsh() -> String {
    let mut script = String::from(
        "#compdef uwe\n# The zsh completion of uwe, from `uwe completions zsh`.\n_uwe() {\n  \
         local line\n  local -a choices options\n  if (( CURRENT == 2 )); then\n",
    );
    let _ = writeln!(
        script,
        "    choices=({})\n    compadd -a choices\n    return\n  fi",
        typed_commands().join(" ")
    );
    script.push_str("  line=$words[2]\n  case $line in\n");
    for group in GROUPS {
        let names: Vec<&str> = subcommands(group).iter().map(|(name, _)| *name).collect();
        let _ = writeln!(
            script,
            "    {group})\n      if (( CURRENT == 3 )); then\n        choices=({})\n        \
             compadd -a choices\n        return\n      fi\n      line=\"$line $words[3]\" ;;",
            names.join(" ")
        );
    }
    script.push_str("  esac\n  case $line in\n");
    for (line, command) in lines() {
        let _ = writeln!(
            script,
            "    \"{line}\") options=({}) ;;",
            spellings(command)
        );
    }
    script.push_str(
        "  esac\n  if [[ $PREFIX == -* ]]; then\n    compadd -a options\n  else\n    \
         _files\n  fi\n}\ncompdef _uwe uwe\n",
    );
    script
}

fn fish() -> String {
    let mut script = String::from("# The fish completion of uwe, from `uwe completions fish`.\n");
    let commands = typed_commands().join(" ");
    let _ = writeln!(
        script,
        "complete -c uwe -n '__fish_use_subcommand' -f -a '{commands}'"
    );
    for group in GROUPS {
        let names: Vec<&str> = subcommands(group).iter().map(|(name, _)| *name).collect();
        let names = names.join(" ");
        let _ = writeln!(
            script,
            "complete -c uwe -n '__fish_seen_subcommand_from {group}; and not \
             __fish_seen_subcommand_from {names}' -f -a '{names}'"
        );
    }
    for (line, command) in lines() {
        let condition = match line.split_once(' ') {
            Some((group, name)) => format!(
                "__fish_seen_subcommand_from {group}; and __fish_seen_subcommand_from {name}"
            ),
            None => format!("__fish_seen_subcommand_from {line}"),
        };
        for option in options_of(command) {
            let name = option.trim_start_matches('-');
            let _ = writeln!(
                script,
                "complete -c uwe -n '{condition}' -o {name} -l {name}"
            );
        }
    }
    script
}
