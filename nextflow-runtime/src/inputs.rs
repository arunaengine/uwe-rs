//! Builds pipeline input schemas from schema files, embedded sample sheets, and declared params.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use input_schema::{DRAFT, PATH, infer, properties, upgrade};
use nextflow_config::Config;
use nextflow_evaluator::lowered::{Module, TypedParameter};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;
use nextflow_parser::lexer::tokenize;
use nextflow_parser::token::{Dialect, Token, TokenKind};
use serde_json::{Map, Value as Json, json};

/// The schema file of a pipeline: `validation.parametersSchema` of the config, else
/// `nextflow_schema.json`, relative to the project directory.
pub fn schema_file(config: &Config, project: &Path) -> PathBuf {
    let name = config
        .get(&["validation", "parametersSchema"])
        .filter(Value::truth)
        .map_or_else(
            || "nextflow_schema.json".to_owned(),
            |name| name.to_string(),
        );
    project.join(name)
}

/// The schema of a `nextflow_schema.json` in draft 2020-12, with the schema of each sample
/// sheet in the place of its name.
pub fn from_file(file: &Path, project: &Path) -> Result<Json, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let schema: Json = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", file.display()))?;
    let mut schema = upgrade(schema);
    embed_sample_sheets(&mut schema, project)?;
    Ok(schema)
}

/// Puts the schema of each file that a `schema` keyword of a text names, like
/// `assets/schema_input.json`, in the place of the name.
fn embed_sample_sheets(schema: &mut Json, project: &Path) -> Result<(), String> {
    match schema {
        Json::Object(map) => {
            let schema_of_text = map.contains_key("type") || map.contains_key("format");
            if schema_of_text && let Some(Json::String(name)) = map.get("schema") {
                let embedded = sample_sheet(name, project)?;
                map.insert("schema".into(), embedded);
                return Ok(());
            }
            for value in map.values_mut() {
                embed_sample_sheets(value, project)?;
            }
        }
        Json::Array(items) => {
            for item in items {
                embed_sample_sheets(item, project)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The schema of a sample sheet by the name that nf-schema takes, relative to the project
/// directory, in draft 2020-12 and without a nested `$schema`. Its `$id` remains the base URI
/// of its references.
fn sample_sheet(name: &str, project: &Path) -> Result<Json, String> {
    let candidates = [
        project.join(name),
        project.join(name.trim_start_matches('/')),
    ];
    let file = candidates
        .iter()
        .find(|file| file.is_file())
        .unwrap_or(&candidates[1]);
    let text = std::fs::read_to_string(file).map_err(|error| {
        format!(
            "cannot read sample-sheet schema {}: {error}",
            file.display()
        )
    })?;
    let schema: Json = serde_json::from_str(&text).map_err(|error| {
        format!(
            "sample-sheet schema {} is not valid JSON: {error}",
            file.display()
        )
    })?;
    let mut schema = upgrade(schema);
    input_schema::valid(&schema).map_err(|error| {
        format!(
            "sample-sheet schema {} is not valid: {error}",
            file.display()
        )
    })?;
    if let Json::Object(map) = &mut schema {
        map.remove("$schema");
    }
    Ok(schema)
}

/// What a pipeline without a schema file declares: the params of the config and of its
/// `params.x = value` statements, without those of the command line, and the params of its
/// typed `params` block with their evaluated defaults.
pub struct Declared<'a> {
    pub params: Value,
    pub typed: &'a [TypedParameter],
    pub defaults: Vec<Option<Value>>,
    /// The name and the description of the pipeline, from its manifest.
    pub title: String,
    pub description: Option<String>,
}

/// The schema of a pipeline without a schema file: each param with the type of its default,
/// and each typed param with its type; a typed param without a default that may not be `null`
/// is required, except a `Boolean`, which is `false` then.
pub fn declared(declared: &Declared, base: &Path) -> Json {
    let defaults = match to_json(&declared.params) {
        Json::Object(map) => map,
        _ => Map::new(),
    };
    let mut schema = infer(&defaults, base);
    let mut required = Vec::new();
    for (index, parameter) in declared.typed.iter().enumerate() {
        let class = parameter.class.as_deref().unwrap_or("Object");
        let mut property = typed_schema(class);
        match declared.defaults.get(index).cloned().flatten() {
            Some(Value::Null) => {}
            Some(default) => property["default"] = to_json(&default),
            None if class != "Boolean" && !parameter.nullable => {
                required.push(json!(parameter.name.to_string()));
            }
            None => {}
        }
        schema["properties"][parameter.name.to_string()] = property;
    }
    schema["title"] = json!(declared.title);
    if let Some(description) = &declared.description {
        schema["description"] = json!(description);
    }
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema["$schema"] = json!(DRAFT);
    schema
}

/// The schema of a param of the typed `params` block by its type. A `Path` may name a file or
/// a directory, which need not exist, as Nextflow converts the text without looking.
fn typed_schema(class: &str) -> Json {
    match class {
        "String" | "Duration" | "MemoryUnit" => json!({"type": "string"}),
        "Integer" | "Long" => json!({"type": "integer"}),
        "Float" | "Double" | "Number" | "BigDecimal" => json!({"type": "number"}),
        "Boolean" => json!({"type": "boolean"}),
        "Path" => json!({"type": "string", "format": PATH}),
        "List" | "Set" | "Bag" => json!({"type": "array"}),
        "Map" => json!({"type": "object"}),
        _ => json!({}),
    }
}

/// The params of a run as nf-schema checks them: a value that Groovy counts as false, like
/// `null`, `false`, `''` or `[]`, is left out unless it is zero, and a map is cleaned inside.
pub fn values(params: &Value) -> Json {
    cleaned(to_json(params)).unwrap_or_else(|| json!({}))
}

fn cleaned(value: Json) -> Option<Json> {
    match value {
        Json::Null | Json::Bool(false) => None,
        Json::String(text) if text.is_empty() => None,
        Json::Array(items) if items.is_empty() => None,
        Json::Object(map) if map.is_empty() => None,
        Json::Object(map) => Some(Json::Object(
            map.into_iter()
                .filter_map(|(key, value)| Some((key, cleaned(value)?)))
                .collect(),
        )),
        other => Some(other),
    }
}

/// A value of a script or config as JSON: paths as their text, durations and memory units as
/// they print, and closures and other objects as `null`.
pub fn to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Boolean(value) => Json::Bool(*value),
        Value::Number(Number::Integer(value)) => json!(value),
        Value::Number(Number::Long(value)) => json!(value),
        Value::Number(number) => {
            serde_json::Number::from_f64(number.to_f64()).map_or(Json::Null, Json::Number)
        }
        Value::String(text) | Value::GString(text) => json!(text.as_ref()),
        Value::List(list) => Json::Array(list.borrow().items.iter().map(to_json).collect()),
        Value::Map(map) => Json::Object(
            map.borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.to_string(), to_json(value)))
                .collect(),
        ),
        Value::Path(path) => match path.prefix {
            None => json!(path.path),
            Some(_) => json!(path.uri()),
        },
        Value::MemoryUnit(_) | Value::Duration(_) | Value::Character(_) => json!(value.to_string()),
        _ => Json::Null,
    }
}

/// Detects included nf-schema or nf-validation checks unless validate_params disables them.
/// Nextflow alone does not validate params against nextflow_schema.json.
pub fn validates(module: &Rc<Module>, params: &Value) -> bool {
    let switched_off = match nextflow_config::tree::get(params, &["validate_params"]) {
        Some(Value::Boolean(false)) => true,
        Some(Value::String(text)) => text.eq_ignore_ascii_case("false"),
        _ => false,
    };
    !switched_off && validates_from(module, &mut Vec::new())
}

fn validates_from(module: &Rc<Module>, seen: &mut Vec<*const Module>) -> bool {
    if seen.contains(&Rc::as_ptr(module)) {
        return false;
    }
    seen.push(Rc::as_ptr(module));
    let includes = module.includes.iter().any(|include| {
        &*include.name == "validateParameters"
            && matches!(
                &*include.source,
                "plugin/nf-schema" | "plugin/nf-validation"
            )
    });
    if includes {
        return true;
    }
    let linked: Vec<Rc<Module>> = (module.links.borrow().values())
        .chain(module.conditional_links.borrow().values())
        .map(|link| link.module.clone())
        .collect();
    linked.iter().any(|module| validates_from(module, seen))
}

/// Whether the check of the pipeline takes types leniently, as nf-schema's
/// `validation.lenientMode` and nf-validation's `params.validationLenientMode` ask.
pub fn lenient(config: &Config) -> bool {
    [
        ["validation", "lenientMode"],
        ["params", "validationLenientMode"],
    ]
    .iter()
    .any(|setting| config.get(setting).is_some_and(|value| value.truth()))
}

/// Reads the numbers and booleans of params whose schema asks for text as text, as nf-schema's
/// lenient mode takes them.
pub fn as_text(schema: &Json, values: &mut Json) {
    let Json::Object(values) = values else {
        return;
    };
    for property in properties(schema) {
        let text = match property.schema.get("type") {
            Some(Json::String(kind)) => kind == "string",
            Some(Json::Array(kinds)) => kinds.iter().any(|kind| kind == "string"),
            _ => false,
        };
        if let Some(value) = values.get_mut(property.name)
            && text
            && matches!(value, Json::Number(_) | Json::Bool(_))
        {
            *value = Json::String(value.to_string());
        }
    }
}

/// Reads ignored params from nf-schema validation.ignoreParams and defaultIgnoreParams,
/// or nf-validation params.validationSchemaIgnoreParams, as comma-separated names.
pub fn ignored(config: &Config) -> Vec<String> {
    let mut names = Vec::new();
    for setting in ["ignoreParams", "defaultIgnoreParams"] {
        if let Some(Value::List(list)) = config.get(&["validation", setting]) {
            names.extend(list.borrow().items.iter().map(Value::to_string));
        }
    }
    if let Some(value) = config.get(&["params", "validationSchemaIgnoreParams"]) {
        let text = value.to_string();
        names.extend(text.split(',').map(|name| name.trim().to_owned()));
    }
    names
}

/// The params that nf-validation reads by their names.
const NF_VALIDATION_PARAMS: &[&str] = &[
    "help",
    "monochrome_logs",
    "validationFailUnrecognisedParams",
    "validationLenientMode",
    "validationS3PathCheck",
    "validationSchemaIgnoreParams",
    "validationShowHiddenParams",
    "validationSkipDuplicateCheck",
];

/// Finds parameter reads in scripts, configs, and Groovy classes, excluding assignments.
/// Includes plugin-read schema params and every schema param read by summary calls.
/// See the crate README for recognized read forms and plugin scope.
pub fn read(project: &Path, schema: &Json) -> BTreeSet<String> {
    let mut files = Vec::new();
    source_files(project, &mut files);
    let mut names = BTreeSet::new();
    let (mut nf_schema, mut nf_validation, mut summary) = (false, false, false);
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let dialect = match file.extension().and_then(|extension| extension.to_str()) {
            Some("config") => Dialect::Config,
            _ => Dialect::Script,
        };
        nf_schema |= source.contains("plugin/nf-schema");
        nf_validation |= source.contains("plugin/nf-validation");
        summary |= ["paramsSummaryLog", "paramsSummaryMap"]
            .iter()
            .any(|call| source.contains(call));
        params_read(&source, dialect, &mut names);
    }
    let mut plugin: Vec<&str> = Vec::new();
    if nf_schema {
        plugin.extend(["help", "help_full", "show_hidden"]);
    }
    if nf_validation {
        plugin.extend(NF_VALIDATION_PARAMS);
    }
    let all = summary && (nf_schema || nf_validation);
    for property in properties(schema) {
        if all || plugin.contains(&property.name) {
            names.insert(property.name.to_owned());
        }
    }
    names
}

/// The files of a project that hold code: `.nf`, `.config` and `.groovy` files, outside hidden
/// folders, the `work` folder of Nextflow, the `tests` folders of nf-test and folders with a
/// `nextflow_schema.json` of their own, which hold another pipeline. A run loads none of them.
fn source_files(folder: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !name.starts_with('.')
                && name != "work"
                && name != "tests"
                && !path.join("nextflow_schema.json").is_file()
            {
                source_files(&path, files);
            }
        } else if [".nf", ".config", ".groovy"]
            .iter()
            .any(|extension| name.ends_with(extension))
        {
            files.push(path);
        }
    }
}

/// The params that one source text reads.
fn params_read(source: &str, dialect: Dialect, names: &mut BTreeSet<String>) {
    let tokens = tokenize(source, dialect).tokens;
    let text = |token: &Token| {
        let (start, end) = (token.span.start as usize, token.span.end as usize);
        source.get(start..end).unwrap_or_default()
    };
    let kind = |index: usize| tokens.get(index).map(|token| token.kind);
    for (index, token) in tokens.iter().enumerate() {
        // Config files have no `params` keyword; there it is a name.
        let params = token.kind == TokenKind::Params
            || (token.kind == TokenKind::Identifier && text(token) == "params");
        match token.kind {
            _ if params => match kind(index + 1) {
                Some(TokenKind::Dot | TokenKind::SafeDot) => {
                    let Some(name) = tokens.get(index + 2).map(text).filter(|n| identifier(n))
                    else {
                        continue;
                    };
                    match kind(index + 3) {
                        // A method, like `params.each { ... }`, reads no param by its name.
                        Some(TokenKind::LeftBrace) => {}
                        Some(TokenKind::LeftParen) => {
                            let method = matches!(name, "get" | "getOrDefault" | "containsKey");
                            if let Some(literal) = tokens.get(index + 4).filter(|_| method) {
                                insert_literal(text(literal), literal.kind, names);
                            }
                        }
                        Some(TokenKind::Assign) => {}
                        _ => {
                            names.insert(name.to_owned());
                        }
                    }
                }
                Some(TokenKind::LeftBracket) => {
                    if let Some(literal) = tokens.get(index + 2) {
                        insert_literal(text(literal), literal.kind, names);
                    }
                }
                _ => {}
            },
            TokenKind::GStringPath => {
                if let Some(rest) = text(token).strip_prefix("$params.") {
                    let name = rest.split('.').next().unwrap_or_default();
                    if identifier(name) {
                        names.insert(name.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
}

/// The name that a string literal holds, like `'outdir'`.
fn insert_literal(text: &str, kind: TokenKind, names: &mut BTreeSet<String>) {
    if kind != TokenKind::StringLiteral {
        return;
    }
    let name = text.trim_matches(|c| c == '\'' || c == '"');
    if identifier(name) {
        names.insert(name.to_owned());
    }
}

fn identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && characters.all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline_schema(name: &str) -> Json {
        json!({
            "$schema": DRAFT,
            "type": "object",
            "properties": {
                "input": {"type": "string", "schema": name}
            }
        })
    }

    #[test]
    fn reports_samplesheet_source() {
        let project = tempfile::tempdir().expect("a folder");
        let assets = project.path().join("assets");
        std::fs::create_dir(&assets).expect("makes assets");
        let pipeline = project.path().join("nextflow_schema.json");
        std::fs::write(
            &pipeline,
            pipeline_schema("assets/missing.json").to_string(),
        )
        .unwrap();
        let missing = assets.join("missing.json");
        let error = from_file(&pipeline, project.path()).unwrap_err();
        assert!(error.contains(&missing.display().to_string()), "{error}");
        assert!(error.contains("cannot read sample-sheet schema"), "{error}");

        let malformed = assets.join("malformed.json");
        std::fs::write(&malformed, "{").unwrap();
        std::fs::write(
            &pipeline,
            pipeline_schema("assets/malformed.json").to_string(),
        )
        .unwrap();
        let error = from_file(&pipeline, project.path()).unwrap_err();
        assert!(error.contains(&malformed.display().to_string()), "{error}");
        assert!(error.contains("is not valid JSON"), "{error}");

        let invalid = assets.join("invalid.json");
        std::fs::write(&invalid, r#"{"type":"words"}"#).unwrap();
        std::fs::write(
            &pipeline,
            pipeline_schema("assets/invalid.json").to_string(),
        )
        .unwrap();
        let error = from_file(&pipeline, project.path()).unwrap_err();
        assert!(error.contains(&invalid.display().to_string()), "{error}");
        assert!(error.contains("is not valid"), "{error}");
    }

    #[test]
    fn preserves_reference_scope() {
        let project = tempfile::tempdir().expect("a folder");
        let assets = project.path().join("assets");
        std::fs::create_dir(&assets).expect("makes assets");
        let pipeline = project.path().join("nextflow_schema.json");
        std::fs::write(&pipeline, pipeline_schema("assets/rows.json").to_string()).unwrap();
        let rows = json!({
            "$schema": DRAFT,
            "$id": "https://example.org/schemas/rows.json",
            "$defs": {"row": {"type": "object", "properties": {"sample": {"type": "string"}}}},
            "type": "array",
            "items": {"$ref": "#/$defs/row"}
        });
        std::fs::write(assets.join("rows.json"), rows.to_string()).unwrap();

        let schema = from_file(&pipeline, project.path()).unwrap();
        let embedded = &schema["properties"]["input"]["schema"];
        assert_eq!(embedded["$id"], rows["$id"]);
        assert!(embedded.get("$schema").is_none());
        assert_eq!(embedded["items"], json!({"$ref": "#/$defs/row"}));
        assert_eq!(input_schema::valid(embedded), Ok(()));
    }

    fn read_in(source: &str, dialect: Dialect) -> Vec<String> {
        let mut names = BTreeSet::new();
        params_read(source, dialect, &mut names);
        names.into_iter().collect()
    }

    #[test]
    fn summarizes_schema_parameters() {
        let project = tempfile::tempdir().expect("a folder");
        let schema = serde_json::json!({
            "properties": {"input": {"type": "string"}, "config_profile_name": {"type": "string"}}
        });
        let main = "include { paramsSummaryLog } from 'plugin/nf-schema'\n\
                    workflow { log.info paramsSummaryLog(workflow); println params.input }\n";
        std::fs::write(project.path().join("main.nf"), main).expect("writes main.nf");
        let found: Vec<String> = read(project.path(), &schema).into_iter().collect();
        assert_eq!(found, ["config_profile_name", "input"]);
        // Without the summary, the plugin reads only its own params.
        let main = "include { validateParameters } from 'plugin/nf-schema'\n\
                    workflow { validateParameters(); println params.input }\n";
        std::fs::write(project.path().join("main.nf"), main).expect("writes main.nf");
        let found: Vec<String> = read(project.path(), &schema).into_iter().collect();
        assert_eq!(found, ["input"]);
    }

    #[test]
    fn excludes_nested_pipelines() {
        let project = tempfile::tempdir().expect("a folder");
        let other = project.path().join("nf-core-demo");
        std::fs::create_dir(&other).expect("makes the folder");
        std::fs::write(
            project.path().join("main.nf"),
            "workflow { println params.greeting }",
        )
        .expect("writes main.nf");
        std::fs::write(other.join("main.nf"), "workflow { println params.input }")
            .expect("writes the other main.nf");
        std::fs::write(other.join("nextflow_schema.json"), "{}").expect("writes the schema");
        let found: Vec<String> = read(project.path(), &json!({})).into_iter().collect();
        assert_eq!(found, ["greeting"]);
    }

    #[test]
    fn combines_parameter_sources() {
        let script = r#"
            params.declared = 'x'
            // params.commented
            workflow {
                if (params.input && params?.safe) { println "${params.outdir}/a $params.prefix.x" }
                def g = params.genomes[params['genome']]
                params.get('fasta'); params.containsKey("gtf"); params.each { println it }
            }
        "#;
        assert_eq!(
            read_in(script, Dialect::Script),
            [
                "fasta", "genome", "genomes", "gtf", "input", "outdir", "prefix", "safe"
            ]
        );
        let config = "params { outdir = 'results'; max = 2 }\nprocess { ext.args = { \"--threads ${params.max}\" } }";
        assert_eq!(read_in(config, Dialect::Config), ["max"]);
    }

    #[test]
    fn normalizes_schema_values() {
        let params = json!({"input": null, "skip": false, "name": "", "zero": 0, "list": [], "map": {"a": null, "b": 1}});
        assert_eq!(cleaned(params), Some(json!({"zero": 0, "map": {"b": 1}})));
    }
}
