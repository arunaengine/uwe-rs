//! Turns the words of a command line into a tool: base command, inputs, redirections, pipes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

use std::path::Path;

use serde_json::{Value as Json, json};

use crate::tool::{Argument, Binding, Input, Kind, Tool};

/// Words that must reach the shell unquoted.
pub(crate) const OPERATORS: &[&str] = &["|", ">", "2>"];

/// Interpreters whose next word, the script, belongs to the base command.
const INTERPRETERS: &[&str] = &["python", "python3", "R", "Rscript", "node", "java"];
/// Interpreter options that move the script one word to the right, as in `python -m module`.
const MODIFIERS: &[&str] = &["-e", "-m"];
/// A positional value with one of these words may be a secret: it gets no default.
const SECRETS: &[&str] = &["sql", "postgres", "mysql", "password"];

/// The tool of a command line, without outputs. `folder` is where the command runs, so
/// words that name a file or folder there become `File` and `Directory` inputs.
pub fn parse(words: &[String], folder: &Path) -> Tool {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let base_command = base(&words);
    let rest = &words[base_command.len()..];
    let (own, piped) = match rest.iter().position(|word| *word == "|") {
        Some(at) => (&rest[..at], &rest[at + 1..]),
        None => (rest, &[][..]),
    };
    let redirect = |operator: &str| {
        let at = own.iter().position(|word| *word == operator)?;
        Some((at, own.get(at + 1).map(|target| (*target).to_owned())))
    };
    let stdout = redirect(">");
    let stderr = redirect("2>");
    let end = [&stdout, &stderr]
        .iter()
        .filter_map(|found| found.as_ref().map(|(at, _)| *at))
        .min()
        .unwrap_or(own.len());
    let inputs = inputs(&own[..end], folder);
    let arguments = arguments(piped, inputs.len());
    let staged = script(&base_command)
        .filter(|script| folder.join(script).is_file())
        .map(str::to_owned);
    Tool {
        base_command,
        arguments,
        inputs,
        outputs: Vec::new(),
        stdout: stdout.and_then(|(_, target)| target),
        stderr: stderr.and_then(|(_, target)| target),
        staged: staged.into_iter().collect(),
        image: None,
    }
}

/// The name of a tool: its script or program, up to the first `.`.
pub fn name(base_command: &[String]) -> String {
    let word = script(base_command)
        .or(base_command.first().map(String::as_str))
        .unwrap_or("tool");
    let file = Path::new(word).file_name().and_then(|file| file.to_str());
    let stem = file.unwrap_or(word).split('.').next().unwrap_or(word);
    match stem.is_empty() {
        true => "tool".to_owned(),
        false => stem.to_owned(),
    }
}

/// The leading words that form the command: `echo`, `python script.py`, `python -m module`.
fn base(words: &[&str]) -> Vec<String> {
    let Some(first) = words.first() else {
        return Vec::new();
    };
    let count = match (interpreter(first), words.get(1)) {
        (true, Some(next)) if words.len() > 2 && modifier(next) => 3,
        (true, Some(_)) => 2,
        _ => 1,
    };
    words[..count]
        .iter()
        .map(|word| (*word).to_owned())
        .collect()
}

/// The script of an interpreter command, the word that names the tool and gets staged.
fn script(base_command: &[String]) -> Option<&str> {
    match base_command {
        [_, flag, script] if modifier(flag) => Some(script),
        [_, script] => Some(script),
        _ => None,
    }
}

/// Whether a word names an interpreter, also with a version like `python3.11`, not `Rake`.
fn interpreter(word: &str) -> bool {
    INTERPRETERS.iter().any(|name| {
        word.strip_prefix(name)
            .is_some_and(|rest| rest.chars().next().is_none_or(|next| !next.is_alphabetic()))
    })
}

fn modifier(word: &str) -> bool {
    MODIFIERS.iter().any(|flag| word.starts_with(flag))
}

/// The inputs of the words between the base command and the first redirection.
fn inputs(words: &[&str], folder: &Path) -> Vec<Input> {
    let mut inputs = Vec::new();
    let mut secrets = 0;
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        let input = match words.get(index + 1) {
            _ if !flag(word) => {
                let (value, kind) = typed(word, folder);
                let secret = SECRETS
                    .iter()
                    .any(|part| value.to_lowercase().contains(part));
                secrets += usize::from(secret);
                Input {
                    id: match secret {
                        true => format!("secret_{secrets}"),
                        false => slug(value),
                    },
                    default: (!secret).then(|| default(value, kind)),
                    format: kind.format(value),
                    kind,
                    binding: Binding::Position(index),
                }
            }
            Some(next) if !flag(next) => {
                index += 1;
                let (value, kind) = typed(next, folder);
                Input {
                    id: slug(word),
                    kind,
                    default: Some(default(value, kind)),
                    binding: Binding::Prefix(word.to_owned()),
                    format: kind.format(value),
                }
            }
            _ => Input {
                id: slug(word),
                kind: Kind::Boolean,
                default: Some(Json::Bool(true)),
                binding: Binding::Prefix(word.to_owned()),
                format: None,
            },
        };
        inputs.push(input);
        index += 1;
    }
    inputs
}

/// Whether a word is an option name. Negative numbers like `-5` are values.
fn flag(word: &str) -> bool {
    word.starts_with('-') && word.parse::<f64>().is_err()
}

/// The value and type of a word. A one-letter hint sets the type, as in `f:data.csv`.
fn typed<'a>(word: &'a str, folder: &Path) -> (&'a str, Kind) {
    let hinted = word.split_once(':').and_then(|(hint, value)| {
        let kind = match hint {
            "f" => Kind::File,
            "d" => Kind::Directory,
            "s" => Kind::String,
            "r" => Kind::Float,
            "i" => Kind::Int,
            "l" => Kind::Long,
            "b" => Kind::Boolean,
            _ => return None,
        };
        Some((value, kind))
    });
    hinted.unwrap_or_else(|| (word, guess(word, folder)))
}

/// The type of a value: a file or folder that exists, an address, a number or text.
fn guess(value: &str, folder: &Path) -> Kind {
    let path = folder.join(value);
    let address = ["http://", "https://", "ftp://", "s3://", "file://"];
    if path.is_file() || address.iter().any(|scheme| value.starts_with(scheme)) {
        Kind::File
    } else if path.is_dir() {
        Kind::Directory
    } else if value == "true" || value == "false" {
        Kind::Boolean
    } else if value.parse::<i64>().is_ok() {
        Kind::Int
    } else if value.parse::<f64>().is_ok_and(f64::is_finite) {
        Kind::Float
    } else {
        Kind::String
    }
}

fn default(value: &str, kind: Kind) -> Json {
    let number = |text: &str| serde_json::from_str(text).unwrap_or(json!(text));
    match kind {
        Kind::File => json!({ "class": "File", "location": value }),
        Kind::Directory => json!({ "class": "Directory", "location": value }),
        Kind::Int | Kind::Long | Kind::Float => number(value),
        Kind::Boolean => json!(value == "true"),
        Kind::String => json!(value),
    }
}

/// The words after a `|`, as literal arguments after the inputs. The tool then needs a shell.
fn arguments(piped: &[&str], after: usize) -> Vec<Argument> {
    if piped.is_empty() {
        return Vec::new();
    }
    let pipe = Argument {
        position: after,
        value: "|".to_owned(),
        quoted: false,
    };
    let rest = piped.iter().enumerate().map(|(index, word)| Argument {
        position: after + 1 + index,
        value: (*word).to_owned(),
        quoted: !OPERATORS.contains(word),
    });
    std::iter::once(pipe).chain(rest).collect()
}

/// An id made of the lowercase letters and digits of a text, joined by `_`.
pub(crate) fn slug(text: &str) -> String {
    let lower = text.to_lowercase();
    let parts: Vec<&str> = lower
        .split(|letter: char| !letter.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect();
    match parts.is_empty() {
        true => "input".to_owned(),
        false => parts.join("_"),
    }
}
