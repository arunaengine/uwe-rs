//! Reads nf-validation CSV and TSV rows, including headerless sheets with an empty field.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::csv::{self, CsvOptions};
use nextflow_evaluator::interpreter::glob::{GlobOptions, glob};
use nextflow_evaluator::interpreter::{Error, parse_yaml};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};

use super::{flag, parameters_schema};
use crate::Context;
use crate::everit::{self, Problem};
use crate::nf_schema::{entries, get, json, path, paths, plain, read, schema, thrown};

/// `fromSamplesheet`: the rows of the samplesheet that a param names, checked and converted
/// by the schema that the param's definition names. Each row is the list of its fields in
/// schema order, with the `meta` fields in a map first.
pub(super) fn from_samplesheet(
    options: Option<&Value>,
    param: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let name = param.to_string();
    let parameters = parameters_schema(options, context)?;
    let skip_duplicates = match options.and_then(|options| get(options, "skip_duplicate_check")) {
        Some(skip) => skip.truth(),
        None => flag(context, "validationSkipDuplicateCheck"),
    };
    let Some(sheet) = get(&context.params, &name).filter(|sheet| !matches!(sheet, Value::Null))
    else {
        return Err(thrown(format!(
            "Parameter '--{name}' was not provided. Unable to create a channel from it."
        )));
    };
    let Some(definition) = find_deep(&parameters.tree, &name) else {
        return Err(thrown(format!(
            "Parameter '--{name}' was not found in the schema. Unable to create a channel from it."
        )));
    };
    let Some(sheet_schema) = get(&definition, "schema") else {
        return Err(thrown(format!(
            "Parameter '--{name}' does not contain a schema in the parameter schema. Unable to \
             create a channel from it."
        )));
    };
    validate_file(&name, &sheet, &sheet_schema, context)?;
    convert(&sheet, &sheet_schema, skip_duplicates, context)
}

/// The value of the first `key` in a map or the maps inside it, as `findDeep` looks.
fn find_deep(value: &Value, key: &str) -> Option<Value> {
    get(value, key).or_else(|| {
        entries(value)
            .into_iter()
            .find_map(|(_, inner)| find_deep(&inner, key))
    })
}

/// A samplesheet read as rows of maps, with whether it has a header.
struct Sheet {
    file: PathValue,
    rows: Vec<Value>,
    header: bool,
    /// The fields of the schema's items, in order.
    fields: Vec<(String, Value)>,
}

fn read_sheet(sheet: &Value, schema_path: &Value, context: &mut Context) -> Result<Sheet, Error> {
    let file = path(sheet, &context.host.launch_dir());
    let schema = schema(schema_path, context)?;
    let fields = get(&schema.tree, "items")
        .and_then(|items| get(&items, "properties"))
        .map_or_else(Vec::new, |properties| entries(&properties));
    let header = !(fields.len() == 1 && fields[0].0.is_empty());
    let text = read(&file, context)?;
    let rows = match file_type(&file, &text)? {
        "yaml" => {
            let items = match parse_yaml(&text).map_err(|error| thrown(error.message))? {
                Value::List(list) => list.borrow().items.clone(),
                _ => Vec::new(),
            };
            items
                .into_iter()
                .map(|item| match header {
                    true => item,
                    false => Value::map([(Value::string("empty"), item)]),
                })
                .collect()
        }
        kind => {
            let options = CsvOptions {
                separator: if kind == "csv" { ',' } else { '\t' },
                quote: Some('"'),
                header: Some((!header).then(|| vec![Value::string("empty")])),
                skip: 0,
                strip: true,
            };
            // The plugin reads the sheet itself; an empty sheet has no rows.
            csv::Records::new(text.clone(), &options).collect()
        }
    };
    Ok(Sheet {
        file,
        rows,
        header,
        fields,
    })
}

/// `csv`, `tsv` or `yaml` by the extension, else by the separator the first line has more of.
fn file_type(file: &PathValue, text: &str) -> Result<&'static str, Error> {
    match file.extension() {
        "csv" => return Ok("csv"),
        "tsv" => return Ok("tsv"),
        "yml" | "yaml" => return Ok("yaml"),
        _ => {}
    }
    let first = text.lines().next().unwrap_or_default();
    let (commas, tabs) = (first.matches(',').count(), first.matches('\t').count());
    match commas.cmp(&tabs) {
        std::cmp::Ordering::Greater => Ok("csv"),
        std::cmp::Ordering::Less => Ok("tsv"),
        std::cmp::Ordering::Equal => Err(thrown(format!(
            "Could not derive file type from {}. Please specify the file extension (CSV, TSV, \
             YML and YAML are supported).",
            file.uri()
        ))),
    }
}

/// Checks a samplesheet against its schema: text becomes a boolean, integer or number where
/// the schema asks for one and it reads as one, and the rows are validated.
pub(super) fn validate_file(
    name: &str,
    sheet: &Value,
    schema_path: &Value,
    context: &mut Context,
) -> Result<(), Error> {
    let read = read_sheet(sheet, schema_path, context)?;
    let schema = schema(schema_path, context)?;
    let rows: Vec<serde_json::Value> =
        read.rows
            .iter()
            .map(|row| {
                let casted = entries(row).into_iter().filter_map(|(key, value)| {
                    let field = if read.header { key.as_str() } else { "" };
                    let kind = read.fields.iter().find(|(name, _)| name == field).map(
                        |(_, definition)| {
                            get(definition, "type").map_or("string".into(), |kind| kind.to_string())
                        },
                    );
                    let value = typed(&value, kind.as_deref())?;
                    Some((key, json(&value)))
                });
                serde_json::Value::Object(casted.collect())
            })
            .collect();
    let instance = serde_json::Value::Array(rows);
    let validator = paths::validator(&schema.json, &instance, context, true)
        .map_err(|error| thrown(format!("Invalid samplesheet schema: {error}")))?;
    let problems: Vec<String> = everit::problems(&validator, &instance)
        .into_iter()
        .map(|problem| match problem {
            Problem::Missing { path, name } => format!(
                "* -- Entry {}: Missing required value: {name}",
                entry(&path).0
            ),
            Problem::Value { path, message, .. } => {
                let (entry, field) = entry(&path);
                let value = instance.pointer(&path).map(plain).unwrap_or_default();
                if message.starts_with("the file or directory") {
                    format!("* -- Entry {entry} - {field}: {message}.")
                } else {
                    format!("* -- Entry {entry} - {field}: {message} ({value})")
                }
            }
        })
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    Err(thrown(format!(
        "The following errors have been detected in --{name} ({}):\n\n{}\n",
        read.file.uri(),
        problems.join("\n")
    )))
}

/// The entry number, counted from 1, and the field of a location like `/0/sample`.
fn entry(path: &str) -> (usize, &str) {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let index = parts.next().and_then(|index| index.parse::<usize>().ok());
    (
        index.map_or(0, |index| index + 1),
        parts.next().unwrap_or_default(),
    )
}

/// A field of a samplesheet for validation, as `castToType` of the validator makes it:
/// `None` for an empty one.
fn typed(value: &Value, kind: Option<&str>) -> Option<Value> {
    let text = match value {
        Value::Null => return None,
        other => other.to_string(),
    };
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    Some(match kind {
        Some("boolean") if lower == "true" || lower == "false" => Value::Boolean(lower == "true"),
        Some("integer") => text
            .parse::<i32>()
            .map_or(Value::string(text.as_str()), Value::integer),
        Some("number") => text
            .parse::<f64>()
            .map_or(Value::string(text.as_str()), |n| {
                Value::Number(Number::Float(n as f32))
            }),
        _ => Value::string(text),
    })
}

/// nf-validation's `convertToList`: each row becomes the list of its fields, with the
/// `meta` fields in a map first. Duplicate rows, fields that must be unique and fields that
/// others need are errors.
fn convert(
    sheet: &Value,
    schema_path: &Value,
    skip_duplicates: bool,
    context: &mut Context,
) -> Result<Value, Error> {
    let read = read_sheet(sheet, schema_path, context)?;
    let launch = context.host.launch_dir();
    let (mut problems, mut warnings) = (Vec::new(), Vec::new());
    let mut seen: Vec<Vec<(String, String)>> = Vec::new();
    let mut uniques: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut result = Vec::new();
    for (index, row) in read.rows.iter().enumerate() {
        let count = index + 1;
        // Empty fields do not count, as if the sheet did not have them.
        let row: Vec<(String, String)> = entries(row)
            .into_iter()
            .filter(|(_, value)| !matches!(value, Value::Null))
            .map(|(key, value)| (key, value.to_string()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let field = |key: &str| row.iter().find(|(name, _)| name == key).map(|(_, v)| v);
        if index == 0 && read.header {
            let unexpected: Vec<&str> = row
                .iter()
                .map(|(key, _)| key.as_str())
                .filter(|key| !read.fields.iter().any(|(name, _)| name == key))
                .collect();
            if !unexpected.is_empty() {
                warnings.push(format!(
                    "The samplesheet contains following unchecked field(s): [{}]",
                    unexpected.join(", ")
                ));
            }
        }
        if !skip_duplicates && let Some(first) = seen.iter().position(|earlier| *earlier == row) {
            problems.push(format!(
                "The samplesheet contains duplicate rows for entry {} and entry {count} ({})",
                first + 1,
                groovy_map(&row)
            ));
        }
        seen.push(row.clone());
        let mut meta = Vec::new();
        let mut output = Vec::new();
        for (name, definition) in &read.fields {
            let key = if read.header { name.as_str() } else { "empty" };
            let input = field(key);
            if let Some(Value::List(needed)) = get(definition, "dependentRequired")
                && input.is_some()
            {
                let needed: Vec<String> =
                    needed.borrow().items.iter().map(Value::to_string).collect();
                let missing: Vec<&String> = needed.iter().filter(|n| field(n).is_none()).collect();
                if !missing.is_empty() {
                    problems.push(format!(
                        "Entry {count}: [{}] field(s) should be defined when '{key}' is \
                         specified, but the field(s) [{}] is/are not defined.",
                        needed.join(", "),
                        missing
                            .iter()
                            .map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            if let Some(unique) = get(definition, "unique").filter(Value::truth) {
                let mut combination = vec![(key.to_owned(), input.cloned().unwrap_or_default())];
                if let Value::List(others) = &unique {
                    for other in others.borrow().items.iter() {
                        let other = other.to_string();
                        combination
                            .push((other.clone(), field(&other).cloned().unwrap_or_default()));
                    }
                }
                let taken = uniques
                    .iter()
                    .any(|(known, values)| known == key && *values == combination);
                if taken && input.is_some() {
                    problems.push(match unique {
                        Value::List(_) => format!(
                            "Entry {count}: The combination of '{key}' with fields {unique} needs \
                             to be unique. {} was found at least twice.",
                            groovy_map(&combination)
                        ),
                        _ => format!(
                            "Entry {count}: The '{key}' value needs to be unique. '{}' was found \
                             at least twice in the samplesheet.",
                            input.cloned().unwrap_or_default()
                        ),
                    });
                }
                uniques.push((key.to_owned(), combination));
            }
            let default = get(definition, "default").filter(|d| !matches!(d, Value::Null));
            let text = input.cloned().or_else(|| default.map(|d| d.to_string()));
            let value = match &text {
                Some(text) => cast(text, definition, &launch, context)?,
                None => Value::Null,
            };
            match get(definition, "meta") {
                Some(Value::List(names)) if !names.borrow().items.is_empty() => {
                    for id in names.borrow().items.iter() {
                        meta.push((Value::string(id.to_string()), value.clone()));
                    }
                }
                _ if text.is_none() => output.push(Value::list(Vec::new())),
                _ => output.push(value),
            }
        }
        if !meta.is_empty() {
            output.insert(0, Value::map(meta));
        }
        result.push(Value::list(output));
    }
    if !problems.is_empty() {
        problems.sort();
        let lines: Vec<String> = problems.iter().map(|p| format!("\t{p}")).collect();
        return Err(thrown(format!("Samplesheet errors:\n{}", lines.join("\n"))));
    }
    if !warnings.is_empty() {
        let lines: Vec<String> = warnings.iter().map(|w| format!("\t{w}")).collect();
        context.host.print(&format!(
            "WARN: Samplesheet warnings:\n{}\n",
            lines.join("\n")
        ));
    }
    Ok(Value::list(result))
}

/// A field as `castToType` of the converter makes it: a path or a list of paths for the
/// path formats, a number, a boolean, or the text.
fn cast(
    text: &str,
    definition: &Value,
    launch: &str,
    context: &mut Context,
) -> Result<Value, Error> {
    let kind = get(definition, "type").map(|kind| kind.to_string());
    let format = get(definition, "format").map(|format| format.to_string());
    Ok(match kind.as_deref() {
        None | Some("string") => match format.as_deref() {
            Some("file-path-pattern") => {
                let files = glob(context.host, text, &GlobOptions::default())?;
                Value::list(
                    files
                        .into_iter()
                        .map(|file| Value::Path(Rc::new(file)))
                        .collect(),
                )
            }
            Some(format) if format.contains("path") => {
                Value::Path(Rc::new(PathValue::parse(text, launch)))
            }
            _ => Value::string(text),
        },
        Some("number") => match text.parse::<i32>() {
            Ok(integer) => Value::integer(integer),
            Err(_) => text.parse::<f32>().map_or(Value::string(text), |float| {
                Value::Number(Number::Float(float))
            }),
        },
        Some("integer") => text
            .parse::<i32>()
            .map_or(Value::string(text), Value::integer),
        Some("boolean") => Value::Boolean(text.eq_ignore_ascii_case("true")),
        _ => Value::Null,
    })
}

/// Fields as Groovy prints a map of them, like `[sample:A, fastq_1:a.fq]`.
fn groovy_map(fields: &[(String, String)]) -> String {
    let fields: Vec<String> = fields.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    format!("[{}]", fields.join(", "))
}
