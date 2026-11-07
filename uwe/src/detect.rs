//! Finds the language of a workflow from its file or directory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::Language;

/// The names Snakemake looks for in a directory, in its order.
const SNAKEFILES: [&str; 4] = [
    "Snakefile",
    "snakefile",
    "workflow/Snakefile",
    "workflow/snakefile",
];

/// Detects Nextflow projects, Snakefiles and CWL, Argo or wir documents.
/// CWL uses `cwlVersion`; Argo uses `apiVersion: argoproj.io`; wir is JSON with `wir_version`.
pub fn language(target: &Path) -> Result<Language, String> {
    let shown = target.display();
    if target.is_dir() {
        let nextflow = ["main.nf", "nextflow.config"]
            .iter()
            .any(|name| target.join(name).is_file());
        let snakemake = SNAKEFILES.iter().any(|name| target.join(name).is_file())
            || has_extension(target, "smk");
        return match (nextflow, snakemake) {
            (true, false) => Ok(Language::Nextflow),
            (false, true) => Ok(Language::Snakemake),
            (true, true) => Err(format!(
                "{shown} holds a Nextflow and a Snakemake workflow; choose one with `-lang`"
            )),
            (false, false) => Err(format!(
                "{shown} holds no workflow uwe knows: no main.nf, nextflow.config, Snakefile \
                 or .smk file; name the workflow file"
            )),
        };
    }
    if !target.is_file() {
        return Err(format!(
            "{shown}: no such file or directory; name a workflow file or a project directory"
        ));
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let extension = target.extension().and_then(|extension| extension.to_str());
    match extension {
        Some("nf") => return Ok(Language::Nextflow),
        Some("smk" | "snakefile") => return Ok(Language::Snakemake),
        Some("cwl") => return Ok(Language::Cwl),
        _ if name == "Snakefile" || name == "snakefile" => return Ok(Language::Snakemake),
        _ => {}
    }
    let text = std::fs::read_to_string(target).map_err(|error| format!("{shown}: {error}"))?;
    let found = json_language(&text).or_else(|| yaml_language(&text));
    found.ok_or(format!(
        "cannot tell the language of {shown}; choose one with `-lang`"
    ))
}

/// The main file of a workflow: the target itself when it is a file; in a project directory,
/// `main.nf` for Nextflow and the Snakefile for Snakemake, else the first file with the
/// language's extension.
pub fn main_file(target: &Path, language: Language) -> Option<PathBuf> {
    if target.is_file() {
        return Some(target.to_owned());
    }
    let (names, extension): (&[&str], &str) = match language {
        Language::Nextflow => (&["main.nf"], "nf"),
        Language::Snakemake => (&SNAKEFILES, "smk"),
        Language::Cwl | Language::Argo | Language::Wir => return None,
    };
    if let Some(name) = names.iter().find(|name| target.join(name).is_file()) {
        return Some(target.join(name));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(target)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|found| found == extension))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// Whether a directory holds a file with the extension.
fn has_extension(directory: &Path, wanted: &str) -> bool {
    std::fs::read_dir(directory).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let path = entry.path();
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == wanted)
        })
    })
}

fn json_language(text: &str) -> Option<Language> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("cwlVersion").is_some() {
        return Some(Language::Cwl);
    }
    if value.get("wir_version").is_some() {
        return Some(Language::Wir);
    }
    let api = value.get("apiVersion")?.as_str()?;
    api.starts_with("argoproj.io/").then_some(Language::Argo)
}

/// The language from the top-level keys of a YAML file, which may hold several documents.
fn yaml_language(text: &str) -> Option<Language> {
    for line in text.lines() {
        // Top-level keys start in the first column.
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim_matches(['"', '\'']);
        let value = value.trim().trim_matches(['"', '\'']);
        match key {
            "cwlVersion" => return Some(Language::Cwl),
            "apiVersion" if value.starts_with("argoproj.io/") => return Some(Language::Argo),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The language of a file with the given name and text, in a directory of its own.
    fn file(name: &str, text: &str) -> Result<Language, String> {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes the directory");
        std::fs::write(&path, text).expect("writes the file");
        language(&path)
    }

    /// The language of a directory holding the given files.
    fn directory(names: &[&str]) -> Result<Language, String> {
        let directory = tempfile::tempdir().expect("a temporary directory");
        for name in names {
            let path = directory.path().join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes a directory");
            std::fs::write(path, "").expect("writes a file");
        }
        language(directory.path())
    }

    #[test]
    fn files_by_name() {
        assert_eq!(file("main.nf", ""), Ok(Language::Nextflow));
        assert_eq!(file("Snakefile", ""), Ok(Language::Snakemake));
        assert_eq!(file("snakefile", ""), Ok(Language::Snakemake));
        assert_eq!(file("rules/align.smk", ""), Ok(Language::Snakemake));
        assert_eq!(file("tool.cwl", ""), Ok(Language::Cwl));
    }

    #[test]
    fn files_by_content() {
        let cwl = "#!/usr/bin/env cwl-runner\ncwlVersion: v1.2\nclass: Workflow\n";
        assert_eq!(file("workflow.yml", cwl), Ok(Language::Cwl));
        let packed = r#"{"$graph": [], "cwlVersion": "v1.0"}"#;
        assert_eq!(file("packed.json", packed), Ok(Language::Cwl));
        let argo = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nspec:\n  entrypoint: main\n";
        assert_eq!(file("hello.yaml", argo), Ok(Language::Argo));
        let argo = r#"{"apiVersion": "argoproj.io/v1alpha1", "kind": "WorkflowTemplate"}"#;
        assert_eq!(file("template.json", argo), Ok(Language::Argo));
        let wir = r#"{"wir_version": "0.1", "graphs": [], "entry": "main"}"#;
        assert_eq!(file("run.wir.json", wir), Ok(Language::Wir));
        let other = "apiVersion: v1\nkind: ConfigMap\n";
        assert!(file("map.yaml", other).is_err());
        assert!(file("job.json", r#"{"reads": "a.fq"}"#).is_err());
    }

    #[test]
    fn detects_directory_language() {
        assert_eq!(directory(&["main.nf"]), Ok(Language::Nextflow));
        assert_eq!(directory(&["nextflow.config"]), Ok(Language::Nextflow));
        assert_eq!(directory(&["Snakefile"]), Ok(Language::Snakemake));
        assert_eq!(
            directory(&["workflow/Snakefile", "config/config.yaml"]),
            Ok(Language::Snakemake)
        );
        assert_eq!(directory(&["pipeline.smk"]), Ok(Language::Snakemake));
        assert!(directory(&["main.nf", "Snakefile"]).is_err());
        assert!(directory(&["README.md"]).is_err());
    }

    #[test]
    fn rejects_missing_targets() {
        assert!(language(Path::new("/no/such/workflow.nf")).is_err());
    }

    #[test]
    fn selects_project_entry() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let project = root.path();
        std::fs::create_dir_all(project.join("workflow")).expect("makes a directory");
        std::fs::write(project.join("workflow/Snakefile"), "").expect("writes a file");
        let found = main_file(project, Language::Snakemake);
        assert_eq!(found, Some(project.join("workflow/Snakefile")));
        std::fs::write(project.join("b.nf"), "").expect("writes a file");
        std::fs::write(project.join("a.nf"), "").expect("writes a file");
        assert_eq!(
            main_file(project, Language::Nextflow),
            Some(project.join("a.nf"))
        );
        std::fs::write(project.join("main.nf"), "").expect("writes a file");
        assert_eq!(
            main_file(project, Language::Nextflow),
            Some(project.join("main.nf"))
        );
        let file = project.join("main.nf");
        assert_eq!(main_file(&file, Language::Nextflow), Some(file.clone()));
        assert_eq!(main_file(project, Language::Cwl), None);
    }
}
