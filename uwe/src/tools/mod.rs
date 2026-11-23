//! The tool library of uwe: tool definitions from Galaxy, CWL, WDL, Nextflow and Snakemake
//! sources and definition files, and composed workflows of them that lower into wir.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod command;
pub mod composed;
mod cwl;
pub mod galaxy;
pub mod library;
pub mod lowering;
mod nextflow;
mod snakemake;
mod wdl;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json};
use tool_definition::{Definition, Source, SourceKind};

pub use composed::{ComposedInput, ComposedOutput, ComposedWorkflow};
pub use library::{Entry, Query, Skipped, ToolLibrary};
pub use lowering::{Lowered, LoweringError, lower};

/// The tool definitions of one file: a definition file (`.tool.json`, `.tool.yaml` or
/// `.tool.yml`, one definition or a list), Galaxy tool XML, a CWL CommandLineTool, the tasks
/// of a WDL document, the processes of a Nextflow script or the rules of a Snakefile.
pub fn extract(path: &Path) -> Result<Vec<Definition>, String> {
    let name =
        (path.file_name()).map_or(String::new(), |name| name.to_string_lossy().to_lowercase());
    if [".tool.json", ".tool.yaml", ".tool.yml"]
        .iter()
        .any(|end| name.ends_with(end))
    {
        return definition_file(path);
    }
    let ending = path
        .extension()
        .map(|ending| ending.to_string_lossy().to_lowercase());
    match ending.as_deref() {
        Some("xml") => {
            let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
            match galaxy_runtime::is_tool(&text) {
                true => galaxy::definitions(path),
                false => Err("the XML file is no Galaxy tool".to_owned()),
            }
        }
        Some("cwl") => cwl::definitions(path),
        Some("wdl") => wdl::definitions(path),
        Some("nf") => nextflow::definitions(path),
        Some("smk") => snakemake::definitions(path),
        Some("ga") => Err("a Galaxy workflow is no tool".to_owned()),
        _ if name == "snakefile" => snakemake::definitions(path),
        _ => Err("uwe reads no tools from this file".to_owned()),
    }
}

/// The ecosystem that the explorer files a tool under: its source language, or `recorded`
/// for a tool that `uwe record` wrote.
pub fn ecosystem(definition: &Definition) -> &'static str {
    let recorded =
        (definition.description.as_deref()).is_some_and(|text| text.starts_with("Recorded from `"));
    match (
        recorded,
        definition.source.as_ref().map(|source| source.kind),
    ) {
        (true, _) => "recorded",
        (_, Some(SourceKind::Galaxy)) => "galaxy",
        (_, Some(SourceKind::Cwl)) => "cwl",
        (_, Some(SourceKind::Wdl)) => "wdl",
        (_, Some(SourceKind::Nextflow | SourceKind::NfCore)) => "nextflow",
        (_, Some(SourceKind::Snakemake)) => "snakemake",
        (_, Some(SourceKind::Container)) => "container",
        _ => "other",
    }
}

/// Whether a scan looks at a file for tools, by its name.
pub(crate) fn is_candidate(path: &Path) -> bool {
    let name =
        (path.file_name()).map_or(String::new(), |name| name.to_string_lossy().to_lowercase());
    let endings = [
        ".tool.json",
        ".tool.yaml",
        ".tool.yml",
        ".xml",
        ".cwl",
        ".wdl",
        ".nf",
        ".smk",
    ];
    name == "snakefile" || endings.iter().any(|end| name.ends_with(end))
}

/// Definitions of a definition file, JSON or YAML, as one object or a list of them.
fn definition_file(path: &Path) -> Result<Vec<Definition>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => tool_definition::yaml::read(&text)?,
    };
    let items = match value {
        Json::Array(items) => items,
        other => vec![other],
    };
    (items.into_iter())
        .map(|item| serde_json::from_value(item).map_err(|error| error.to_string()))
        .collect()
}

/// Where a tool comes from: the file, at the SHA-256 digest of its bytes.
pub(crate) fn source(kind: SourceKind, path: &Path) -> Result<Source, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let digest = task_model::Digest::sha256(&bytes);
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    Ok(Source {
        kind,
        address: path.to_string_lossy().into_owned(),
        revision: format!("sha256:{}", digest.hex()),
    })
}

/// The tag of an image name, like `0.12.1--hdfd78af_0` of `biocontainers/fastqc:0.12.1--hdfd78af_0`.
pub(crate) fn tag(image: &str) -> Option<String> {
    let name = image.split('@').next().unwrap_or(image);
    let last = name.rsplit('/').next().unwrap_or(name);
    let (_, tag) = last.split_once(':')?;
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// The wir document of a wir file as `uwe lower` prints it: a composed workflow lowered with
/// source locations from `base` and its generated files written below `generated`, a wir
/// document in YAML, or one in JSON as it is.
pub fn document(target: &Path, base: &Path, generated: &Path) -> Result<wir::Document, String> {
    match composed_kind(target)? {
        Kind::Composed => {
            let mut library = ToolLibrary::new();
            let workflow = composed::load(target, &mut library)?;
            let lowered = lower(&workflow, &library, (base, generated))
                .map_err(|error| format!("{}: {error}", target.display()))?;
            write_files(&lowered.files)?;
            Ok(lowered.document)
        }
        Kind::Wir(document) => Ok(*document),
        Kind::Other => {
            let text = std::fs::read_to_string(target)
                .map_err(|error| format!("{}: {error}", target.display()))?;
            wir::parse(&text).map_err(|error| format!("{}: {error}", target.display()))
        }
    }
}

/// Writes files that lowering made, each through a temporary file beside it that is renamed
/// into place, so that no reader sees a part of one.
pub(crate) fn write_files(files: &[(PathBuf, String)]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for (file, text) in files {
        let failed = |error: std::io::Error| format!("cannot write {}: {error}", file.display());
        let folder = file.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(folder).map_err(failed)?;
        let mut staged = (tempfile::Builder::new().prefix(".").suffix(".part"))
            .permissions(std::fs::Permissions::from_mode(0o644))
            .tempfile_in(folder)
            .map_err(failed)?;
        std::io::Write::write_all(&mut staged, text.as_bytes()).map_err(failed)?;
        staged.persist(file).map_err(|error| failed(error.error))?;
    }
    Ok(())
}

/// A wir document that uwe wrote for a composed workflow or a wir document in YAML.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The JSON wir document.
    pub path: PathBuf,
    /// The folder that its relative locations start from: the folder of the given file.
    pub base: PathBuf,
    /// The JSON Schema keywords of its entry ports, like their descriptions.
    pub annotations: BTreeMap<String, Map<String, Json>>,
}

/// Writes the wir document of `target` below `folder` when it is a composed workflow or a wir
/// document in YAML, with the files that lowering made; `None` for other files.
pub fn prepare(target: &Path, folder: &Path) -> Result<Option<Prepared>, String> {
    let shown = target.display();
    let base = std::path::absolute(target).unwrap_or_else(|_| target.to_owned());
    let base = base.parent().unwrap_or(Path::new("/")).to_owned();
    let (document, annotations) = match composed_kind(target)? {
        Kind::Composed => {
            let mut library = ToolLibrary::new();
            let workflow = composed::load(target, &mut library)?;
            let generated = folder.join("generated");
            let lowered = lower(&workflow, &library, (&base, &generated))
                .map_err(|error| format!("{shown}: {error}"))?;
            write_files(&lowered.files)?;
            (lowered.document, lowered.annotations)
        }
        Kind::Wir(document) => (*document, BTreeMap::new()),
        Kind::Other => return Ok(None),
    };
    let text = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
    let digest = task_model::Digest::sha256(format!("{}\n{text}", base.display()).as_bytes());
    let path = folder.join(format!("{}.wir.json", digest.hex()));
    (std::fs::create_dir_all(folder).and_then(|()| std::fs::write(&path, text)))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(Some(Prepared {
        path,
        base,
        annotations,
    }))
}

enum Kind {
    Composed,
    Wir(Box<wir::Document>),
    Other,
}

/// What a file holds: a composed workflow by its name or its `uwe_version`, a wir document in
/// YAML by its `wir_version`, or something else, like a wir document in JSON.
fn composed_kind(target: &Path) -> Result<Kind, String> {
    if composed::is_composed(target) {
        return Ok(Kind::Composed);
    }
    let ending = target
        .extension()
        .map(|ending| ending.to_string_lossy().to_lowercase());
    if !matches!(ending.as_deref(), Some("yaml" | "yml")) {
        return Ok(Kind::Other);
    }
    let text = std::fs::read_to_string(target)
        .map_err(|error| format!("{}: {error}", target.display()))?;
    if text.lines().any(|line| line.starts_with("uwe_version:")) {
        return Ok(Kind::Composed);
    }
    match composed::wir_document(&text).map_err(|error| format!("{}: {error}", target.display()))? {
        Some(document) => Ok(Kind::Wir(Box::new(document))),
        None => Ok(Kind::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_tags() {
        assert_eq!(
            tag("biocontainers/fastqc:0.12.1--hdfd78af_0").as_deref(),
            Some("0.12.1--hdfd78af_0")
        );
        assert_eq!(tag("localhost:5000/tool").as_deref(), None);
        assert_eq!(tag("debian@sha256:ab").as_deref(), None);
    }
}
