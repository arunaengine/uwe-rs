//! The groups of commands, like `uwe runs show`, and the flat commands that they stand for.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::options::COMMANDS;

/// A group of commands: its subcommands with the command that each one is, and the command
/// of the group alone.
pub(super) struct Group {
    pub(super) name: &'static str,
    subcommands: &'static [(&'static str, &'static str)],
    alone: Option<&'static str>,
}

/// The groups of commands; `doctor` is `check-machine`.
pub(super) const GROUPS: &[Group] = &[
    Group {
        name: "runs",
        subcommands: &[
            ("list", "runs"),
            ("show", "runs show"),
            ("logs", "runs logs"),
            ("cancel", "runs cancel"),
            ("compare", "compare"),
            ("export", "export"),
            ("repeat", "repeat"),
            ("why", "why"),
            ("time", "time"),
            ("report", "report"),
            ("changes", "changes"),
            ("reconcile", "reconcile"),
            ("resume", "resume"),
            ("replay", "replay"),
            ("telemetry", "telemetry"),
            ("origin", "origin"),
        ],
        alone: Some("runs"),
    },
    Group {
        name: "tool",
        subcommands: &[
            ("list", "tool list"),
            ("inspect", "tool inspect"),
            ("extract", "tool extract"),
            ("record", "record"),
            ("check", "tool check"),
            ("suggest", "tool suggest"),
            ("run", "tool run"),
            ("lower", "lower"),
        ],
        alone: Some("tool list"),
    },
    Group {
        name: "cache",
        subcommands: &[("clean", "clean"), ("digest", "digest")],
        alone: None,
    },
    Group {
        name: "doctor",
        subcommands: &[],
        alone: Some("check-machine"),
    },
];

/// The command of a grouped command line, like `compare` for `uwe runs compare a b`, with the
/// words after its subcommand. Other command lines stay as they are.
pub fn ungroup(command: &str, rest: &[String]) -> Result<(String, Vec<String>), String> {
    let Some(Group {
        subcommands, alone, ..
    }) = GROUPS.iter().find(|group| group.name == command)
    else {
        return Ok((command.to_owned(), rest.to_vec()));
    };
    let names: Vec<&str> = subcommands.iter().map(|(name, _)| *name).collect();
    match rest
        .first()
        .filter(|word| !word.starts_with('-') && !subcommands.is_empty())
    {
        Some(word) => match subcommands.iter().find(|(name, _)| name == word) {
            Some((_, flat)) => Ok(((*flat).to_owned(), rest[1..].to_vec())),
            None => Err(format!(
                "unknown command `uwe {command} {word}`; use {}",
                names.join(", ")
            )),
        },
        None => match alone {
            Some(flat) => Ok(((*flat).to_owned(), rest.to_vec())),
            None => Err(format!("use `uwe {command} <{}>`", names.join("|"))),
        },
    }
}

/// The help of a group of commands: its subcommands and the command each one is.
pub fn group_help(command: &str) -> Option<String> {
    let Group {
        subcommands, alone, ..
    } = GROUPS.iter().find(|group| group.name == command)?;
    let mut help = format!("usage: uwe {command}");
    help.push_str(match (subcommands.is_empty(), alone) {
        (true, _) => " [options]",
        (false, Some(_)) => " [<subcommand>] [options]",
        (false, None) => " <subcommand> [options]",
    });
    if let Some(alone) = alone {
        let same = subcommands.iter().find(|(_, flat)| flat == alone);
        let shown = same.map_or((*alone).to_owned(), |(name, _)| format!("{command} {name}"));
        help.push_str(&format!("\n\n`uwe {command}` alone is `uwe {shown}`."));
    }
    if !subcommands.is_empty() {
        help.push_str("\n\nsubcommands:\n");
        for (name, flat) in *subcommands {
            help.push_str(&format!(
                "  {name:<10} `uwe help {flat}` lists its options\n"
            ));
        }
    }
    Some(help)
}

/// The commands as the first word of a command line, with the groups, for shell completion.
pub(crate) fn typed_commands() -> Vec<&'static str> {
    let flat = COMMANDS
        .iter()
        .copied()
        .filter(|command| !command.contains(' '));
    let mut typed: Vec<&str> = flat.chain(GROUPS.iter().map(|group| group.name)).collect();
    typed.sort_unstable();
    typed.dedup();
    typed
}

/// The subcommands of a group, with the command that each one is.
pub(crate) fn subcommands(group: &str) -> &'static [(&'static str, &'static str)] {
    let found = GROUPS.iter().find(|known| known.name == group);
    found.map_or(&[], |known| known.subcommands)
}
