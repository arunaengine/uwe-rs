//! The recorded CWL CommandLineTool, the pass that links its parts, and its YAML text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
// Moved from SciWIn (https://github.com/fairagro/sciwin), Copyright (c) 2025 FAIRagro,
// MIT OR Apache-2.0; see LICENSE-SCIWIN.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value as Json;

/// A small offline table of EDAM formats by file ending.
const FORMATS: &[(&str, &str)] = &[
    ("jpg", "format_3579"),
    ("png", "format_3603"),
    ("svg", "format_3604"),
    ("py", "format_3996"),
    ("json", "format_3464"),
    ("txt", "format_2330"),
    ("yaml", "format_3750"),
    ("csv", "format_3752"),
    ("xlsx", "format_3620"),
];
const EDAM: &str = "http://edamontology.org/";

/// A CWL CommandLineTool as the recorder writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub base_command: Vec<String>,
    /// Literal words after the inputs, like a piped command.
    pub arguments: Vec<Argument>,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    /// Files the tool needs in its working folder, like its script.
    pub staged: Vec<String>,
    /// The container image of the `DockerRequirement` hint.
    pub image: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub position: usize,
    pub value: String,
    /// False for shell operators, which must reach the shell as they are.
    pub quoted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: String,
    pub kind: Kind,
    /// The value of the recorded run, as CWL writes it: `{ class: File, location }` for files.
    pub default: Option<Json>,
    pub binding: Binding,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    Position(usize),
    Prefix(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub id: String,
    pub kind: Kind,
    pub glob: String,
    pub format: Option<String>,
}

/// The CWL type of an input or output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    String,
    Int,
    Long,
    Float,
    Boolean,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::File => "File",
            Kind::Directory => "Directory",
            Kind::String => "string",
            Kind::Int => "int",
            Kind::Long => "long",
            Kind::Float => "float",
            Kind::Boolean => "boolean",
        }
    }

    /// The `edam:` format of a file of this path, by its ending, if the table has it.
    pub fn format(self, path: &str) -> Option<String> {
        let ending = Path::new(path).extension()?.to_str()?;
        let found = FORMATS.iter().find(|(known, _)| *known == ending);
        (self == Kind::File)
            .then_some(found?)
            .map(|(_, term)| format!("edam:{term}"))
    }
}

impl Output {
    /// The output of a file or folder that the run wrote, by its path in the folder.
    pub fn found(path: &Path, folder: bool) -> Output {
        let glob = path.to_string_lossy().replace('\\', "/");
        let kind = if folder { Kind::Directory } else { Kind::File };
        let lower = glob.to_lowercase();
        let trimmed = lower.trim_start_matches(|letter: char| !letter.is_alphabetic());
        let base = if trimmed.is_empty() { &lower } else { trimmed };
        Output {
            id: base.trim_end_matches('/').replace(['.', '/'], "_"),
            format: kind.format(&glob),
            kind,
            glob,
        }
    }
}

impl Tool {
    /// Links the parts after the run: a value of the run that names an output, `stdout` or
    /// an argument becomes a reference to its input, and every id becomes unique.
    pub fn finish(&mut self) {
        let mut seen = BTreeSet::new();
        for input in &mut self.inputs {
            let base = input.id.clone();
            let mut count = 1;
            while !seen.insert(input.id.clone()) {
                count += 1;
                input.id = format!("{base}_{count}");
            }
        }
        for input in &self.inputs {
            let Some(value) = input.default.as_ref().and_then(text) else {
                continue;
            };
            let reference = match input.kind {
                Kind::File | Kind::Directory => format!("$(inputs.{}.path)", input.id),
                _ => format!("$(inputs.{})", input.id),
            };
            let named = (self.outputs.iter_mut().map(|output| &mut output.glob))
                .chain(self.stdout.as_mut())
                .chain(self.stderr.as_mut())
                .chain(
                    self.arguments
                        .iter_mut()
                        .map(|argument| &mut argument.value),
                );
            for place in named.filter(|place| **place == value) {
                place.clone_from(&reference);
            }
        }
        for output in &mut self.outputs {
            if seen.contains(&output.id) {
                output.id = format!("o_{}", output.id);
            }
            seen.insert(output.id.clone());
        }
    }

    /// The tool as a CWL v1.2 YAML document.
    pub fn render(&self) -> String {
        let mut text = String::from("#!/usr/bin/env cwl-runner\ncwlVersion: v1.2\n");
        text.push_str("class: CommandLineTool\n");
        // Only outputs get a format: an input with one refuses files that have none.
        if self.outputs.iter().any(|output| output.format.is_some()) {
            let _ = writeln!(text, "$namespaces:\n  edam: {}", scalar(EDAM));
        }
        if !self.arguments.is_empty() || !self.staged.is_empty() {
            text.push_str("requirements:\n");
        }
        if !self.arguments.is_empty() {
            text.push_str("  ShellCommandRequirement: {}\n");
        }
        if !self.staged.is_empty() {
            text.push_str("  InitialWorkDirRequirement:\n    listing:\n");
            for file in &self.staged {
                let file = scalar(file);
                let _ = writeln!(text, "      - entryname: {file}");
                let _ = writeln!(text, "        entry:\n          $include: {file}");
            }
        }
        if let Some(image) = &self.image {
            let image = scalar(image);
            let _ = writeln!(
                text,
                "hints:\n  DockerRequirement:\n    dockerPull: {image}"
            );
        }
        let words: Vec<String> = self.base_command.iter().map(|word| scalar(word)).collect();
        let _ = writeln!(text, "baseCommand: [{}]", words.join(", "));
        if !self.arguments.is_empty() {
            text.push_str("arguments:\n");
        }
        for argument in &self.arguments {
            let _ = writeln!(text, "  - position: {}", argument.position);
            let _ = writeln!(text, "    valueFrom: {}", scalar(&argument.value));
            if !argument.quoted {
                text.push_str("    shellQuote: false\n");
            }
        }
        text.push_str(if self.inputs.is_empty() {
            "inputs: {}\n"
        } else {
            "inputs:\n"
        });
        for input in &self.inputs {
            let _ = writeln!(
                text,
                "  {}:\n    type: {}",
                key(&input.id),
                input.kind.name()
            );
            match &input.default {
                Some(Json::Object(fields)) => {
                    text.push_str("    default:\n");
                    for (name, value) in fields {
                        let _ = writeln!(text, "      {}: {}", key(name), value_text(value));
                    }
                }
                Some(value) => {
                    let _ = writeln!(text, "    default: {}", value_text(value));
                }
                None => {}
            }
            let _ = match &input.binding {
                Binding::Position(at) => writeln!(text, "    inputBinding:\n      position: {at}"),
                Binding::Prefix(flag) => {
                    writeln!(text, "    inputBinding:\n      prefix: {}", scalar(flag))
                }
            };
        }
        text.push_str(if self.outputs.is_empty() {
            "outputs: {}\n"
        } else {
            "outputs:\n"
        });
        for output in &self.outputs {
            let _ = writeln!(
                text,
                "  {}:\n    type: {}",
                key(&output.id),
                output.kind.name()
            );
            if let Some(format) = &output.format {
                let _ = writeln!(text, "    format: {format}");
            }
            let _ = writeln!(
                text,
                "    outputBinding:\n      glob: {}",
                scalar(&output.glob)
            );
        }
        for (name, target) in [("stdout", &self.stdout), ("stderr", &self.stderr)] {
            if let Some(target) = target {
                let _ = writeln!(text, "{name}: {}", scalar(target));
            }
        }
        text
    }
}

/// The text of a default that a glob, `stdout` or an argument may repeat.
fn text(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Number(number) => Some(number.to_string()),
        Json::Object(fields) => fields.get("location")?.as_str().map(str::to_owned),
        _ => None,
    }
}

fn value_text(value: &Json) -> String {
    match value {
        Json::String(text) => scalar(text),
        other => other.to_string(),
    }
}

/// A YAML key: plain when it is a simple name, else quoted.
fn key(name: &str) -> String {
    let simple = name.starts_with(|letter: char| letter.is_ascii_alphabetic() || letter == '_')
        && name
            .chars()
            .all(|letter| letter.is_ascii_alphanumeric() || "_-".contains(letter));
    if simple {
        name.to_owned()
    } else {
        quoted(name)
    }
}

/// A YAML string: plain when YAML reads it back as the same text, else quoted.
fn scalar(text: &str) -> String {
    let words = [
        "true", "false", "null", "yes", "no", "on", "off", "y", "n", "~",
    ];
    let safe = text.starts_with(|letter: char| letter.is_ascii_alphanumeric() || letter == '_')
        && (text.chars()).all(|letter| letter.is_ascii_alphanumeric() || "_-./".contains(letter))
        && text.parse::<f64>().is_err()
        && !words.contains(&text.to_lowercase().as_str());
    if safe { text.to_owned() } else { quoted(text) }
}

/// A double-quoted YAML string; JSON string escapes are valid YAML.
fn quoted(text: &str) -> String {
    Json::String(text.to_owned()).to_string()
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
