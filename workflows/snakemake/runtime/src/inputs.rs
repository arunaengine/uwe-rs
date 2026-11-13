//! Describes config, validated tables, PEP projects, and read config keys as JSON Schema.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fancy_regex::Regex;
use input_schema::{DIRECTORY_PATH, DRAFT, FILE_PATH, infer, is_remote, upgrade};
use python_evaluator::{Error, Interpreter, Value};
use serde_json::{Map, Value as Json, json};
use workflow_graph::Inputs;

use crate::workflow::State;

/// A `validate(data, schema)` of the workflow, which says what its data looks like.
#[derive(Clone, Debug)]
pub struct Validation {
    pub target: Validated,
    pub schema: Json,
    /// The config as the validation passed it, with the defaults it set; the workflow code may
    /// change the config afterwards.
    pub values: Option<Json>,
}

/// What a `validate` checks.
#[derive(Clone, Debug)]
pub enum Validated {
    /// The config of the workflow.
    Config,
    /// A table, like a sample sheet read with pandas, with its columns.
    Table(Vec<String>),
}

/// The PEP project of the workflow: the file of `pepfile:` and that of `pepschema:`.
#[derive(Clone, Debug, Default)]
pub struct Pep {
    pub file: String,
    pub schema: Option<String>,
}

/// What the inputs of an evaluated workflow come from, taken from its state, so that the
/// state is free while the config files are read again.
pub struct Workflow {
    config: Value,
    configfiles: Vec<String>,
    workdir: PathBuf,
    validations: Vec<Validation>,
    pep: Option<Pep>,
    main_snakefile: PathBuf,
    included: Vec<PathBuf>,
}

impl Workflow {
    pub fn of(state: &State) -> Workflow {
        Workflow {
            config: state.config.clone(),
            configfiles: (state.configfiles.iter())
                .chain(&state.overwrite_configfiles)
                .cloned()
                .collect(),
            workdir: state.workdir.clone(),
            validations: state.validations.clone(),
            pep: state.pep.clone(),
            main_snakefile: state.main_snakefile.clone(),
            included: state.included.clone(),
        }
    }
}

/// The inputs of an evaluated workflow.
pub fn inputs(interpreter: &mut Interpreter, state: &Workflow) -> Result<Inputs, Error> {
    let workdir = state.workdir.clone();
    // The config as the last `validate` of it saw it, else as the Snakefile left it.
    let validated =
        (state.validations.iter().rev()).find_map(|validation| validation.values.clone());
    let mut values = match validated {
        Some(values) => values,
        None => crate::schema::to_json(interpreter, &state.config)?,
    };
    let mut schema = config_schema(interpreter, state)?;
    for validation in &state.validations {
        if let Validated::Table(columns) = &validation.target {
            attach_table(&mut schema, &values, columns, &validation.schema, &workdir);
        }
    }
    if let Some(pep) = &state.pep {
        attach_pep(&mut schema, &mut values, pep, &workdir);
    }
    mark_paths(&mut schema, &values, &workdir);
    if schema.get("title").is_none() {
        let folder = state.main_snakefile.parent().and_then(workflow_folder);
        schema["title"] = json!(folder.unwrap_or_default());
    }
    Ok(Inputs {
        schema,
        values,
        base: workdir,
        read: Some(read(state)),
        assumes_values: false,
        // Snakemake has held the config to the schemas of `validate` while it evaluated the
        // Snakefile, before this check; a schema made from the config files only describes it.
        advisory: true,
        ignored: Default::default(),
    })
}

/// The name of the folder of a workflow: that of its Snakefile, or the one above a `workflow`
/// folder.
pub(crate) fn workflow_folder(folder: &Path) -> Option<String> {
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_owned());
    let folder = folder.as_path();
    let folder = match folder.file_name()?.to_str()? {
        "workflow" => folder.parent()?,
        _ => folder,
    };
    Some(folder.file_name()?.to_string_lossy().into_owned())
}

/// The schema of the config: the schemas of the `validate(config, ...)` calls, else one from
/// the values of the config files, without the values of the command line.
fn config_schema(interpreter: &mut Interpreter, state: &Workflow) -> Result<Json, Error> {
    let schemas: Vec<Json> = state
        .validations
        .iter()
        .filter(|validation| matches!(validation.target, Validated::Config))
        .map(|validation| embedded(validation.schema.clone()))
        .collect();
    Ok(match schemas.as_slice() {
        [] => {
            let mut declared = Map::new();
            for file in &state.configfiles {
                if let Ok(loaded) = crate::config::load_config(interpreter, file) {
                    merge(&mut declared, crate::schema::to_json(interpreter, &loaded)?);
                }
            }
            infer(&declared, &state.workdir)
        }
        [one] => {
            let mut schema = one.clone();
            schema["$schema"] = json!(DRAFT);
            schema
        }
        many => json!({"$schema": DRAFT, "allOf": many}),
    })
}

/// A schema in draft 2020-12 without the `$schema` and `$id` of a file of its own, so that it
/// can sit inside another.
fn embedded(schema: Json) -> Json {
    let mut schema = upgrade(schema);
    if let Json::Object(map) = &mut schema {
        map.remove("$schema");
        map.remove("$id");
    }
    schema
}

/// Merges config values as Snakemake's `update_config` does: mappings recursively, other
/// values replace.
fn merge(target: &mut Map<String, Json>, source: Json) {
    let Json::Object(source) = source else {
        return;
    };
    for (key, value) in source {
        match (target.get_mut(&key), value) {
            (Some(Json::Object(inner)), Json::Object(value)) => merge(inner, Json::Object(value)),
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}

/// Makes the config value that names the file of a validated table a file whose content is an
/// array of rows of that table: the value whose file has a header with most of its columns.
fn attach_table(schema: &mut Json, config: &Json, columns: &[String], rows: &Json, base: &Path) {
    let mut best: Option<(usize, Vec<String>)> = None;
    let mut candidates = Vec::new();
    texts(config, &mut Vec::new(), &mut candidates);
    for (path, text) in candidates {
        let Some(header) = header(text, base) else {
            continue;
        };
        let shared = columns
            .iter()
            .filter(|column| header.contains(column))
            .count();
        // At least half of the columns of the table are in the header.
        if shared > 0
            && shared * 2 >= columns.len()
            && best.as_ref().is_none_or(|(most, _)| shared > *most)
        {
            best = Some((shared, path));
        }
    }
    let Some((_, path)) = best else {
        return;
    };
    let property = property_at(schema, &path);
    property["schema"] = json!({"type": "array", "items": embedded(rows.clone())});
    mark_file(property);
}

/// Every text of a config with the keys that lead to it.
fn texts<'a>(value: &'a Json, path: &mut Vec<String>, found: &mut Vec<(Vec<String>, &'a str)>) {
    match value {
        Json::String(text) => found.push((path.clone(), text)),
        Json::Object(map) => {
            for (key, value) in map {
                path.push(key.clone());
                texts(value, path, found);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The column names of a local table file: the fields of its first line, split at tabs, else
/// at commas.
fn header(text: &str, base: &Path) -> Option<Vec<String>> {
    if text.is_empty() || is_remote(text) {
        return None;
    }
    let file = base.join(text);
    if !file.is_file() {
        return None;
    }
    let content = std::fs::read(&file).ok()?;
    let first = content.split(|byte| *byte == b'\n').next()?;
    let first = String::from_utf8_lossy(first);
    let separator = if first.contains('\t') { '\t' } else { ',' };
    Some(
        first
            .trim_end_matches('\r')
            .split(separator)
            .map(|field| field.trim().trim_matches('"').to_owned())
            .collect(),
    )
}

/// The schema of the config value at a path of keys, made where the schema has none: in the
/// top `properties`, or in the part of `allOf` that has the first key.
fn property_at<'a>(schema: &'a mut Json, path: &[String]) -> &'a mut Json {
    let Some((first, rest)) = path.split_first() else {
        return schema;
    };
    let in_part = (schema.get("allOf").and_then(Json::as_array))
        .and_then(|parts| {
            parts
                .iter()
                .position(|part| part.pointer(&format!("/properties/{first}")).is_some())
        })
        .filter(|_| schema.pointer(&format!("/properties/{first}")).is_none());
    let owner = match in_part {
        Some(index) => &mut schema["allOf"][index],
        None => schema,
    };
    if !owner.get("properties").is_some_and(Json::is_object) {
        owner["properties"] = json!({});
    }
    let property = &mut owner["properties"][first.as_str()];
    if property.is_null() {
        *property = match rest.is_empty() {
            true => json!({"type": "string"}),
            false => json!({"type": "object"}),
        };
    }
    property_at(property, rest)
}

/// Marks a schema as that of a file that must exist, unless it says what it is already.
fn mark_file(schema: &mut Json) {
    if schema.get("format").is_none() {
        schema["format"] = json!(FILE_PATH);
    }
    if schema.get("exists").is_none() {
        schema["exists"] = json!(true);
    }
}

/// The PEP project as the input `pepfile`: the config value of that name when the workflow
/// takes the project from its config, else an input that the workflow sets itself. The
/// schema of `pepschema:` is the schema of its content.
fn attach_pep(schema: &mut Json, values: &mut Json, pep: &Pep, base: &Path) {
    let from_config = values.get("pepfile").and_then(Json::as_str) == Some(pep.file.as_str());
    let property = property_at(schema, &["pepfile".to_owned()]);
    mark_file(property);
    if !from_config {
        property["readOnly"] = json!(true);
        property["default"] = json!(pep.file);
        property["description"] = json!("The PEP project of the samples, which `pepfile:` names");
        values["pepfile"] = json!(pep.file);
    }
    let content = (pep.schema.as_deref())
        .filter(|file| !is_remote(file))
        .and_then(|file| crate::schema::read_schema(&base.join(file)).ok());
    if let Some(content) = content {
        property["schema"] = embedded(content);
    }
}

/// Marks the texts of the schema whose values name existing files or directories as files and
/// directories, where the schema does not say what they are. Whether they must exist is left to
/// the schema, as Snakemake has read what it needs while it evaluated the workflow.
fn mark_paths(schema: &mut Json, values: &Json, base: &Path) {
    let Json::Object(values) = values else {
        return;
    };
    for (name, value) in values {
        let Some(property) = schema
            .get_mut("properties")
            .and_then(|properties| properties.get_mut(name))
        else {
            continue;
        };
        match value {
            Json::String(text) => mark_path(property, text, base),
            Json::Object(_) => mark_paths(property, value, base),
            _ => {}
        }
    }
    if let Some(Json::Array(parts)) = schema.get_mut("allOf") {
        for part in parts {
            mark_paths(part, &Json::Object(values.clone()), base);
        }
    }
}

fn mark_path(property: &mut Json, text: &str, base: &Path) {
    let plain_text = property.get("type").is_none_or(|kind| kind == "string");
    if !plain_text
        || property.get("format").is_some()
        || property.get("enum").is_some()
        || text.is_empty()
        || is_remote(text)
    {
        return;
    }
    match std::fs::metadata(base.join(text)) {
        Ok(metadata) if metadata.is_dir() => property["format"] = json!(DIRECTORY_PATH),
        Ok(_) => property["format"] = json!(FILE_PATH),
        Err(_) => {}
    }
}

/// Finds config reads in Snakefiles, scripts, notebooks, shell interpolation, and R expressions.
/// Includes lookup dpath keys; see the crate README for recognized access forms.
fn read(state: &Workflow) -> BTreeSet<String> {
    let mut files: Vec<PathBuf> = state.included.clone();
    files.push(state.main_snakefile.clone());
    if let Some(folder) = state.main_snakefile.parent() {
        source_files(folder, &mut files);
    }
    files.sort();
    files.dedup();
    let patterns = [
        r#"(?<![\w.])(?:snakemake\.)?config\s*\[\[?\s*["']([^"'\n]+)["']"#,
        r#"(?<![\w.])(?:snakemake\.)?config\.(?:get|setdefault|pop)\(\s*["']([^"'\n]+)["']"#,
        r#"["']([^"'\n]+)["']\s+(?:not\s+)?in\s+config\b"#,
        r"\{config\[([^\]\['\x22\s]+)\]",
        r"snakemake@config\[\[\s*[\x22']([^\x22'\n]+)[\x22']",
        r#"dpath\s*=\s*f?["']([^"'/{\n]+)"#,
    ];
    let patterns: Vec<Regex> = patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect();
    let mut names = BTreeSet::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let line = line.split(" #").next().unwrap_or_default();
            if line.trim_start().starts_with('#') {
                continue;
            }
            for pattern in &patterns {
                for found in pattern.captures_iter(line).flatten() {
                    if let Some(name) = found.get(1) {
                        names.insert(name.as_str().to_owned());
                    }
                }
            }
        }
    }
    names
}

/// The biggest file that the scan of the code reads, which leaves out data files.
const LARGEST_SOURCE: u64 = 1 << 20;

/// The code files of a workflow folder: Snakefiles, rule files, Python and R scripts and
/// notebooks, outside hidden folders.
fn source_files(folder: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if !name.starts_with('.') {
                source_files(&path, files);
            }
            continue;
        }
        let code = name == "Snakefile"
            || [".smk", ".snakefile", ".py", ".R", ".r", ".Rmd", ".ipynb"]
                .iter()
                .any(|extension| name.ends_with(extension));
        if code && metadata.len() <= LARGEST_SOURCE {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaches_config_tables() {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(
            folder.path().join("samples.tsv"),
            "sample\tfq1\tfq2\nA\ta\tb\n",
        )
        .expect("a table");
        let config = json!({"samples": "samples.tsv", "units": "missing.tsv"});
        let mut schema = json!({"properties": {"samples": {"type": "string"}}});
        let row = json!({"type": "object", "properties": {"sample": {"type": "string"}}});
        let columns = ["sample", "fq1", "fq2"].map(str::to_owned);
        attach_table(&mut schema, &config, &columns, &row, folder.path());
        assert_eq!(
            schema["properties"]["samples"],
            json!({
                "type": "string",
                "format": "file-path",
                "exists": true,
                "schema": {"type": "array", "items": row}
            })
        );
    }

    #[test]
    fn infers_unspecified_paths() {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("genome.fa"), ">a\n").expect("a file");
        std::fs::create_dir(folder.path().join("data")).expect("a folder");
        let mut schema = json!({"properties": {
            "genome": {"type": "string"},
            "data": {},
            "mode": {"type": "string", "enum": ["genome.fa"]},
            "nested": {"type": "object", "properties": {"ref": {"type": "string"}}}
        }});
        let values = json!({"genome": "genome.fa", "data": "data", "mode": "genome.fa", "nested": {"ref": "genome.fa"}});
        mark_paths(&mut schema, &values, folder.path());
        assert_eq!(schema["properties"]["genome"]["format"], "file-path");
        assert_eq!(schema["properties"]["data"]["format"], "directory-path");
        assert!(schema["properties"]["mode"].get("format").is_none());
        assert_eq!(
            schema["properties"]["nested"]["properties"]["ref"]["format"],
            "file-path"
        );
    }
}
