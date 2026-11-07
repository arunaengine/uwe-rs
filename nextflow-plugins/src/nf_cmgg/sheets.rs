//! Collects the published files and metrics of each sample, and writes nf-cmgg sample sheets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use nextflow_evaluator::csv::{CsvOptions, records};
use nextflow_evaluator::interpreter::{dump_yaml, parse_json, parse_yaml};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

use super::worksheet::{Computed, Sheet, Type, Worksheet, map_get};

/// The fields of one sample, in the order they were set; output fields hold lists of files.
type Entry = Vec<(String, Value)>;

/// What the sample sheets are made of, gathered during the run.
pub(super) struct Collector {
    worksheet: Worksheet,
    params: Value,
    /// The rows of the input sample sheet by sample ID.
    rows: HashMap<String, Value>,
    /// The sample IDs of the input sample sheet.
    ids: Vec<String>,
    /// The entries by sample ID, which is also the order of the rows.
    entries: BTreeMap<String, Entry>,
    location: PathBuf,
}

impl Collector {
    pub(super) fn new(
        worksheet: Worksheet,
        params: Value,
        input: Option<&Path>,
        location: PathBuf,
    ) -> Collector {
        let rows = input.map(read_sheet).unwrap_or_default();
        let mut collector = Collector {
            worksheet,
            params,
            rows: HashMap::new(),
            ids: Vec::new(),
            entries: BTreeMap::new(),
            location,
        };
        for row in rows {
            let Some(id) = map_get(&row, &collector.worksheet.id_field) else {
                continue;
            };
            let id = id.to_string();
            collector.ids.push(id.clone());
            collector.rows.entry(id).or_insert(row);
        }
        collector
    }

    /// Adds a published file: to the first output field that matches its name, and the values
    /// of every metrics field that matches it. An error ends the handling of the file.
    pub(super) fn published(&mut self, target: &str) -> Result<(), String> {
        let name = target.rsplit('/').next().unwrap_or(target).to_owned();
        let output = (self.worksheet.outputs.iter())
            .find(|output| output.pattern.matches(&name).unwrap_or(false))
            .map(|output| output.name.clone());
        if let Some(field) = output {
            let id = self.sample(&name)?;
            let entry = self.ensure(&id)?;
            let mut files = match lookup(entry, &field) {
                Some(Value::List(list)) => list.borrow().items.clone(),
                _ => Vec::new(),
            };
            files.push(Value::string(target));
            set(entry, &field, Value::list(files));
        }
        let metrics: Vec<(String, PathBuf, String, String, String)> =
            (self.worksheet.metrics.iter())
                .filter(|metric| metric.pattern.find(&name).unwrap_or(false))
                .map(|metric| {
                    let mut path = PathBuf::from(target);
                    if let Some(subpath) = &metric.subpath {
                        path.push(subpath);
                    }
                    let filetype = metric.filetype.clone();
                    (
                        metric.name.clone(),
                        path,
                        filetype,
                        metric.id.clone(),
                        metric.field.clone(),
                    )
                })
                .collect();
        for (field, path, filetype, id_column, value_column) in metrics {
            let rows = read_rows(&path, &filetype).map_err(|message| {
                format!(
                    "Failed to read metrics field '{field}' from '{}': {message}",
                    path.display()
                )
            })?;
            for row in rows {
                let id = map_get(&row, &id_column)
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                let value = map_get(&row, &value_column).unwrap_or(Value::Null);
                let entry = self.ensure(&id)?;
                set(entry, &field, value);
            }
        }
        Ok(())
    }

    /// The sample of a file name: the longest sample ID it starts with, after a leading `snp_`,
    /// else the part before its first `.`, `_` or `-`.
    fn sample(&self, name: &str) -> Result<String, String> {
        let stripped = name.strip_prefix("snp_").unwrap_or(name);
        let mut found: Vec<&String> = self
            .ids
            .iter()
            .filter(|id| stripped.starts_with(id.as_str()))
            .collect();
        found.dedup();
        match found.as_slice() {
            [one] => Ok((*one).clone()),
            [] => {
                let end = stripped.find(['.', '_', '-']).unwrap_or(0);
                if end == 0 {
                    return Err(format!("Cannot parse sample name from path: {name}"));
                }
                let id = stripped[..end].to_owned();
                tracing::warn!(
                    "Could not find sample for path '{name}' in input samplesheet, using '{id}' as sample name"
                );
                Ok(id)
            }
            several => {
                let longest = several
                    .iter()
                    .max_by_key(|id| id.len())
                    .map(|id| (*id).clone());
                let longest = longest.unwrap_or_default();
                let all: Vec<&str> = several.iter().map(|id| id.as_str()).collect();
                tracing::warn!(
                    "Multiple possible samples found for path '{name}': [{}], using '{longest}' as sample name, because it is the longest match",
                    all.join(", ")
                );
                Ok(longest)
            }
        }
    }

    /// The entry of a sample, made from its input row, the input defaults and the values.
    fn ensure(&mut self, id: &str) -> Result<&mut Entry, String> {
        if !self.entries.contains_key(id) {
            let row = self.rows.get(id);
            let mut entry: Entry = vec![("id".into(), Value::string(id))];
            for input in &self.worksheet.inputs {
                let raw = row.and_then(|row| map_get(row, &input.source));
                let value = match raw {
                    None => input.default.clone(),
                    Some(raw) => convert(&raw, input.kind).map_err(|message| {
                        format!(
                            "Failed to convert input field '{}' to {}: {message}",
                            input.name,
                            input.kind.name()
                        )
                    })?,
                };
                set(&mut entry, &input.name, value);
            }
            for (name, computed) in &self.worksheet.values {
                let value = match computed {
                    Computed::Constant(value) => value.clone(),
                    Computed::Function(code) => code
                        .evaluate(&as_map(&entry), &self.params)
                        .map_err(|message| {
                            format!("Failed to evaluate values field '{name}': {message}")
                        })?,
                };
                set(&mut entry, name, value);
            }
            self.entries.insert(id.to_owned(), entry);
        }
        self.entries
            .get_mut(id)
            .ok_or_else(|| format!("no entry for {id}"))
    }

    /// Writes the sample sheets, one folder per value of the `split_by` field when it is set.
    pub(super) fn write(&self) {
        let mut groups: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
        for entry in self.entries.values() {
            let group = match &self.worksheet.split_by {
                None => String::new(),
                Some(field) => match lookup(entry, field).map(|value| value.to_string()) {
                    Some(text) if !text.is_empty() => text,
                    _ => "undefined".into(),
                },
            };
            groups.entry(group).or_default().push(entry);
        }
        for (group, entries) in &groups {
            let directory = match group_folder(&self.location, group) {
                Ok(directory) => directory,
                Err(message) => {
                    tracing::error!("Skipping samplesheets of '{group}': {message}");
                    continue;
                }
            };
            for sheet in &self.worksheet.sheets {
                tracing::info!("Publishing samplesheet '{}'", sheet.name);
                if let Err(message) = self.write_sheet(sheet, entries, &directory) {
                    tracing::error!(
                        "Skipping samplesheet '{}' due to an error: {message}",
                        sheet.name
                    );
                }
            }
        }
    }

    fn write_sheet(
        &self,
        sheet: &Sheet,
        entries: &[&Entry],
        directory: &Path,
    ) -> Result<(), String> {
        let mut passed = Vec::new();
        let mut failed = Vec::new();
        for entry in entries {
            if let Some(include) = &sheet.include
                && !include.evaluate(&as_map(entry), &self.params)?.truth()
            {
                continue;
            }
            for row in expand(entry) {
                let keep = match &sheet.filter {
                    Some(filter) => filter.evaluate(&as_map(&row), &self.params)?.truth(),
                    None => true,
                };
                let projected = project(sheet, &row);
                if keep {
                    passed.push(projected)
                } else {
                    failed.push(projected)
                }
            }
        }
        write_yaml(&directory.join(&sheet.name), passed)?;
        if sheet.filter.is_some() {
            let (base, extension) = sheet.name.rsplit_once('.').unwrap_or((&sheet.name, ""));
            let name = match extension {
                "" => format!("{base}_failed"),
                extension => format!("{base}_failed.{extension}"),
            };
            write_yaml(&directory.join(name), failed)?;
        }
        Ok(())
    }
}

/// The folder of a `split_by` group below the sample sheet folder. The group comes from the
/// input, so an absolute path or a `..` part is refused.
fn group_folder(location: &Path, group: &str) -> Result<PathBuf, String> {
    let relative = Path::new(group);
    let inside = (relative.components()).all(|part| {
        matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    match inside {
        true if group.is_empty() => Ok(location.to_owned()),
        true => Ok(location.join(relative)),
        false => Err("the split_by value is not a plain folder name".into()),
    }
}

/// The rows of an entry: a field with several values gives a row for each, in sorted order;
/// several such fields go together by position. A field with one value holds that value.
fn expand(entry: &Entry) -> Vec<Entry> {
    let mut single = entry.clone();
    let mut multiple: Vec<(usize, Vec<Value>)> = Vec::new();
    for (index, (_, value)) in entry.iter().enumerate() {
        if let Value::List(list) = value {
            let mut items = list.borrow().items.clone();
            items.sort_by_key(|item| item.to_string());
            match items.len() {
                1 => single[index].1 = items.remove(0),
                0 => {}
                _ => multiple.push((index, items)),
            }
        }
    }
    let Some(count) = multiple.iter().map(|(_, items)| items.len()).min() else {
        return vec![single];
    };
    (0..count)
        .map(|row| {
            let mut expanded = single.clone();
            for (index, items) in &multiple {
                expanded[*index].1 = items[row].clone();
            }
            expanded
        })
        .collect()
}

/// The fields of a sample sheet from a row, cast to their types; null values are left out.
fn project(sheet: &Sheet, row: &Entry) -> Value {
    let fields = sheet.fields.iter().filter_map(|field| {
        let value = lookup(row, &field.source).filter(|value| !matches!(value, Value::Null))?;
        let value = match field.kind {
            None => value,
            Some(kind) => cast(&value, kind),
        };
        Some((Value::string(field.key.as_str()), value))
    });
    Value::map(fields.collect::<Vec<_>>())
}

/// Groovy's `value as <type>`.
fn cast(value: &Value, kind: Type) -> Value {
    let text = value.to_string();
    match kind {
        Type::String => Value::string(text),
        Type::Integer => match value {
            Value::Number(number) => Value::integer(number.to_f64() as i32),
            _ => text
                .trim()
                .parse::<i32>()
                .map_or(Value::Null, Value::integer),
        },
        Type::Float => text
            .trim()
            .parse::<f32>()
            .map_or(Value::Null, |f| Value::Number(Number::Float(f))),
        Type::Boolean => Value::Boolean(value.truth()),
    }
}

/// An input cell as its field's type, as Groovy's `toInteger()` and similar read text.
fn convert(raw: &Value, kind: Type) -> Result<Value, String> {
    let text = raw.to_string();
    let invalid = || format!("For input string: \"{text}\"");
    match kind {
        Type::String => Ok(Value::string(text.as_str())),
        Type::Integer => text
            .trim()
            .parse::<i32>()
            .map(Value::integer)
            .map_err(|_| invalid()),
        Type::Float => (text.trim().parse::<f32>())
            .map(|f| Value::Number(Number::Float(f)))
            .map_err(|_| invalid()),
        Type::Boolean => {
            let lowered = text.trim().to_lowercase();
            Ok(Value::Boolean(matches!(
                lowered.as_str(),
                "true" | "y" | "1"
            )))
        }
    }
}

/// Writes a list of rows as block YAML; an empty list is written as SnakeYAML writes it there.
fn write_yaml(path: &Path, rows: Vec<Value>) -> Result<(), String> {
    let text = match rows.is_empty() {
        true => "[\n  ]\n".to_owned(),
        false => dump_yaml(&Value::list(rows)),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

/// The rows of a sample sheet or metrics file by its type: CSV and TSV with a header line,
/// YAML and JSON as a list of maps.
fn read_rows(path: &Path, filetype: &str) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let separator = match filetype {
        "csv" => ',',
        "tsv" => '\t',
        "yml" | "yaml" | "json" => {
            let parsed = match filetype {
                "json" => parse_json(&text),
                _ => parse_yaml(&text),
            };
            return match parsed.map_err(|error| error.message)? {
                Value::List(list) => Ok(list.borrow().items.clone()),
                _ => Ok(Vec::new()),
            };
        }
        other => return Err(format!("Unsupported file format: {other}")),
    };
    let options = CsvOptions {
        separator,
        quote: Some('"'),
        header: Some(None),
        skip: 0,
        strip: true,
    };
    records(&text, &options, false).map_err(|error| error.message)
}

/// The rows of the input sample sheet, with the rows of the sheets that `sample_info` names.
fn read_sheet(path: &Path) -> Vec<Value> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !["csv", "tsv", "yml", "yaml", "json"].contains(&extension.as_str()) {
        tracing::warn!("Unsupported file format: {extension}");
        return Vec::new();
    }
    let mut rows = match read_rows(path, &extension) {
        Ok(rows) => rows,
        Err(_) => {
            tracing::warn!("Could not read samplesheet: file://{}", path.display());
            return Vec::new();
        }
    };
    let mut extra = Vec::new();
    for row in &rows {
        if let Some(info) = map_get(row, "sample_info").map(|info| info.to_string())
            && !info.is_empty()
        {
            let base = path.parent().unwrap_or(Path::new("/"));
            extra.extend(read_sheet(&base.join(info)));
        }
    }
    rows.extend(extra);
    rows
}

fn lookup(entry: &Entry, key: &str) -> Option<Value> {
    entry
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn set(entry: &mut Entry, key: &str, value: Value) {
    match entry.iter_mut().find(|(name, _)| name == key) {
        Some((_, slot)) => *slot = value,
        None => entry.push((key.to_owned(), value)),
    }
}

fn as_map(entry: &Entry) -> Value {
    Value::map(
        entry
            .iter()
            .map(|(key, value)| (Value::string(key.as_str()), value.clone()))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use nextflow_evaluator::value::Value;

    use super::{Entry, expand};

    #[test]
    fn expands_file_lists() {
        let files = |names: &[&str]| Value::list(names.iter().map(|n| Value::string(*n)).collect());
        let entry: Entry = vec![
            ("bam".into(), files(&["a.bam"])),
            ("fastq".into(), files(&["b_R2.fq", "b_R1.fq"])),
        ];
        let rows: Vec<String> = expand(&entry)
            .iter()
            .map(|row| format!("{} {}", row[0].1, row[1].1))
            .collect();
        assert_eq!(rows, ["a.bam b_R1.fq", "a.bam b_R2.fq"]);
    }
}
