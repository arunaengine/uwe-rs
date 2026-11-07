//! The files that the RO-Crate of nf-prov copies next to its metadata: README, diagram, main
//! script, parameter schema, and the resolved config as Nextflow's config printer writes it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value;

use super::wrroc::{Builder, reference};
use super::{file_name, media_type, save};
use crate::report_format::Json;

const READMES: [&str; 7] = [
    "README.md",
    "README.txt",
    "readme.md",
    "readme.txt",
    "Readme.md",
    "Readme.txt",
    "README",
];

impl Builder<'_> {
    /// Copies the README, the diagram, the main script, the parameter schema and the resolved
    /// config into the crate.
    pub(super) fn copy_project(&mut self, main: &str) -> std::io::Result<()> {
        let start = self.run.start;
        let project = Path::new(&start.project_dir);
        if let Some(readme) = READMES.iter().find(|name| project.join(name).is_file()) {
            self.copy(&project.join(readme), readme)?;
            let format = if readme.ends_with(".md") {
                "text/markdown"
            } else {
                "text/plain"
            };
            self.files.push(Json::object([
                ("@id", Json::from(*readme)),
                ("@type", "File".into()),
                ("name", (*readme).into()),
                ("description", "The README file of the workflow.".into()),
                ("about", reference("./")),
                ("encodingFormat", format.into()),
            ]));
        }
        if let Some(diagram) = start.manifest["diagram"].as_str() {
            if project.join(diagram).is_file() {
                self.copy(&project.join(diagram), diagram)?;
                self.ids.insert("diagram".into(), diagram.to_owned());
                self.files.push(Json::object([
                    ("@id", Json::from(diagram)),
                    ("@type", Json::list(["File", "ImageObject"])),
                    ("name", file_name(diagram).into()),
                    ("description", "The diagram of the workflow.".into()),
                    (
                        "encodingFormat",
                        media_type(&project.join(diagram).to_string_lossy()).into(),
                    ),
                ]));
            } else {
                tracing::warn!(
                    "Workflow diagram `{diagram}` does not exist in the project directory -- it \
                     will not be included in the Workflow Run RO-Crate"
                );
            }
        }
        self.copy(Path::new(&start.script), main)?;
        let schema = Path::new(&start.script).with_file_name("nextflow_schema.json");
        if schema.is_file() {
            self.copy(&schema, "nextflow_schema.json")?;
            self.files.push(Json::object([
                ("@id", Json::from("nextflow_schema.json")),
                ("@type", "File".into()),
                ("name", "nextflow_schema.json".into()),
                (
                    "description",
                    "The parameter schema of the workflow.".into(),
                ),
                ("encodingFormat", "application/json".into()),
            ]));
        }
        let resolved = self.folder.join("nextflow.config");
        let loaded =
            (start.config_files.iter()).filter_map(|file| Path::new(file).canonicalize().ok());
        if let Ok(target) = resolved.canonicalize()
            && loaded.into_iter().any(|file| file == target)
        {
            let message = format!(
                "Unable to overwrite existing provenance file: {}",
                resolved.display()
            );
            return Err(std::io::Error::other(message));
        }
        save(&resolved, &config_text(&start.config))?;
        self.files.push(Json::object([
            ("@id", Json::from("nextflow.config")),
            ("@type", "File".into()),
            ("name", "Resolved Nextflow configuration".into()),
            (
                "description",
                "The resolved Nextflow configuration for the workflow run.".into(),
            ),
            ("encodingFormat", "text/plain".into()),
        ]));
        Ok(())
    }

    /// Copies a file into the crate, unless it is already there.
    pub(super) fn copy(&self, source: &Path, relative: &str) -> std::io::Result<()> {
        let target = self.folder.join(relative);
        let same = (source.canonicalize().ok()).zip(target.canonicalize().ok());
        if same.is_some_and(|(source, target)| source == target) {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        copy_tree(source, &target, &self.folder)
    }
}

/// Copies a file, or a folder with all it holds, leaving out the crate folder `skip` and
/// linked folders.
fn copy_tree(source: &Path, target: &Path, skip: &Path) -> std::io::Result<()> {
    if !source.is_dir() {
        return std::fs::copy(source, target).map(|_| ());
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let linked_folder = entry.file_type()?.is_symlink() && path.is_dir();
        if linked_folder || path.canonicalize().ok() == skip.canonicalize().ok() {
            continue;
        }
        copy_tree(&path, &target.join(entry.file_name()), skip)?;
    }
    Ok(())
}

/// The config as Nextflow's canonical config printer writes it: scopes sorted by name, three
/// spaces a level, blank lines around the blocks of the top level.
fn config_text(config: &Value) -> String {
    let mut text = String::new();
    let Some(entries) = config.as_object() else {
        return text;
    };
    let mut last_block = false;
    for (index, (key, value)) in entries
        .iter()
        .filter(|(_, value)| filled(value))
        .enumerate()
    {
        let block = value.is_object();
        if index > 0 && (block || last_block) {
            text.push('\n');
        }
        config_entry(&mut text, key, value, 0);
        last_block = block;
    }
    text
}

fn config_entry(text: &mut String, key: &str, value: &Value, level: usize) {
    let indent = "   ".repeat(level);
    match value {
        Value::Object(entries) => {
            text.push_str(&format!("{indent}{key} {{\n"));
            for (key, value) in entries.iter().filter(|(_, value)| filled(value)) {
                config_entry(text, key, value, level + 1);
            }
            text.push_str(&format!("{indent}}}\n"));
        }
        other => text.push_str(&format!("{indent}{key} = {}\n", config_value(other))),
    }
}

/// Whether a config value is printed: not null, and not an empty scope.
fn filled(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(entries) => !entries.is_empty(),
        _ => true,
    }
}

fn config_value(value: &Value) -> String {
    match value {
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(config_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(entries) => {
            let entries: Vec<String> = (entries.iter())
                .map(|(key, value)| format!("{key}:{}", config_value(value)))
                .collect();
            format!("[{}]", entries.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    /// A folder input lands in the crate with its files, without the crate inside it.
    #[test]
    fn copies_folders() {
        let project = tempfile::tempdir().expect("a folder");
        let input = project.path().join("reads");
        std::fs::create_dir_all(input.join("lane")).expect("makes folders");
        std::fs::write(input.join("lane/a.fq"), "@a\n").expect("writes");
        let crate_folder = input.join("crate");
        std::fs::create_dir_all(&crate_folder).expect("makes the crate");
        let target = crate_folder.join("reads");
        super::copy_tree(&input, &target, &crate_folder).expect("copies");
        let copied = std::fs::read_to_string(target.join("lane/a.fq")).expect("the copy");
        assert_eq!(copied, "@a\n");
        assert!(!target.join("crate").exists());
    }
}
