//! `uwe init`: writes a starter composed workflow with one tool, its inputs and a text file,
//! and leaves every file that exists as it is.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::arguments::Arguments;
use crate::{Failure, failure};

/// The tool of the starter: a CWL CommandLineTool that counts the lines of a text file.
const TOOL: &str = r#"#!/usr/bin/env cwl-runner
cwlVersion: v1.2
class: CommandLineTool
label: Count lines
doc: Counts the lines of a text file.
hints:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [wc, -l]
stdin: $(inputs.text.path)
inputs:
  text:
    type: File
stdout: count.txt
outputs:
  count:
    type: stdout
"#;

/// `uwe init [<folder>] [-name <name>]`: the starter workflow `<name>.uwe.yaml` in the folder,
/// by default the current one, with `tools/count_lines.cwl`, `inputs.json` and `example.txt`.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    if !arguments.targets.is_empty() {
        return Err(Failure::usage(
            "uwe: use `uwe init [<folder>] [-name <name>]`",
        ));
    }
    let folder = arguments
        .target
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let name = match &arguments.tool_name {
        Some(name) => name.clone(),
        None => folder_name(&folder),
    };
    if name.is_empty()
        || !(name.chars()).all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(Failure::usage(format!(
            "uwe: `{name}` is no workflow name; use letters, digits, `-` and `_`"
        )));
    }
    let files = [
        (format!("{name}.uwe.yaml"), workflow(&name)),
        ("tools/count_lines.cwl".to_owned(), TOOL.to_owned()),
        ("inputs.json".to_owned(), inputs()),
        (
            "example.txt".to_owned(),
            "first line\nsecond line\n".to_owned(),
        ),
    ];
    let existing: Vec<String> = (files.iter())
        .filter(|(path, _)| folder.join(path).exists())
        .map(|(path, _)| folder.join(path).display().to_string())
        .collect();
    if !existing.is_empty() {
        return Err(failure(format!(
            "uwe: init writes nothing, as these files exist: {}",
            existing.join(", ")
        )));
    }
    for path in create(&folder, &files)? {
        println!("uwe: wrote {}", path.display());
    }
    let workflow = folder.join(format!("{name}.uwe.yaml"));
    println!(
        "uwe: try `uwe run -executor simulator {} -inputs {}`",
        workflow.display(),
        folder.join("inputs.json").display()
    );
    Ok(())
}

/// Writes each file only where none is, as another process may write one since the check, and
/// removes the files that it wrote when a later one fails, so that `init` writes all or none.
fn create(folder: &Path, files: &[(String, String)]) -> Result<Vec<PathBuf>, Failure> {
    let mut created = Vec::new();
    for (path, text) in files {
        let path = folder.join(path);
        let written =
            (path.parent().map(std::fs::create_dir_all).unwrap_or(Ok(()))).and_then(|()| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                created.push(path.clone());
                file.write_all(text.as_bytes())
            });
        if let Err(error) = written {
            for path in &created {
                let _ = std::fs::remove_file(path);
            }
            return Err(failure(format!(
                "uwe: cannot write {}: {error}",
                path.display()
            )));
        }
    }
    Ok(created)
}

/// The name of a folder, as a name of a workflow; `workflow` for one without a usable name.
fn folder_name(folder: &Path) -> String {
    let absolute = std::path::absolute(folder).unwrap_or_else(|_| folder.to_owned());
    let name = absolute
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let name: String = (name.unwrap_or_default().chars())
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match name.trim_matches('_') {
        "" => "workflow".to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

fn workflow(name: &str) -> String {
    format!(
        r#"# A workflow composed of tools, as `uwe run` runs it; `docs/tools.md` of uwe describes it.
uwe_version: "1"
name: {name}
description: Counts the lines of each text file.
tools:
  - tools
inputs:
  texts:
    type: file
    list: true
    format: format_2330
    description: The text files to count.
steps:
  count:
    tool: count_lines
    in:
      text: texts
outputs:
  counts:
    from: count/count
    description: The number of lines of each file.
"#
    )
}

fn inputs() -> String {
    let value = serde_json::json!({"texts": [{"class": "File", "location": "example.txt"}]});
    format!(
        "{}\n",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_whole_scaffold() {
        let root = tempfile::tempdir().unwrap();
        let file = |path: &str| (path.to_owned(), format!("{path}\n"));
        std::fs::write(root.path().join("taken.txt"), "kept\n").unwrap();
        let files = [
            file("first.txt"),
            file("tools/second.txt"),
            file("taken.txt"),
        ];
        assert!(create(root.path(), &files).is_err());
        assert!(!root.path().join("first.txt").exists());
        assert!(!root.path().join("tools/second.txt").exists());
        let kept = std::fs::read_to_string(root.path().join("taken.txt")).unwrap();
        assert_eq!(kept, "kept\n", "a file that was there stays");
        let written = create(root.path(), &files[..2]).unwrap();
        assert_eq!(written.len(), 2);
    }
}
