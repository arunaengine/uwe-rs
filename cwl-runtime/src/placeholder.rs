//! Placeholder values for inputs that nothing sets, so that a graph builds without a job
//! file: files named after the input, with the extension that their format, their secondary
//! files or their name suggest, since expressions often take names apart. Every rule about
//! placeholders lives here: the values themselves, which optional inputs get them, and which
//! files count as placeholders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use cwl_parser::model::{Expression, InputParameter, Process, SecondaryFile, Type, short_name};

use crate::inputs::has_code;
use crate::scope::Scope;
use crate::types::Types;
use crate::value::{Map, Value};

/// EDAM formats and the extension of their files.
const FORMATS: [(&str, &str); 16] = [
    ("format_1930", ".fastq"),
    ("format_1929", ".fa"),
    ("format_2572", ".bam"),
    ("format_2573", ".sam"),
    ("format_3462", ".cram"),
    ("format_3016", ".vcf"),
    ("format_3003", ".bed"),
    ("format_2306", ".gtf"),
    ("format_1975", ".gff3"),
    ("format_2305", ".gff"),
    ("format_3475", ".tsv"),
    ("format_3752", ".csv"),
    ("format_3464", ".json"),
    ("format_3987", ".zip"),
    ("format_3989", ".gz"),
    ("format_2330", ".txt"),
];

/// Secondary file patterns and the extension of the primary file they belong to.
const SECONDARY: [(&str, &str); 10] = [
    (".bai", ".bam"),
    (".crai", ".cram"),
    (".fai", ".fa"),
    (".dict", ".fa"),
    (".bwt", ".fa"),
    (".amb", ".fa"),
    (".tbi", ".vcf.gz"),
    (".csi", ".vcf.gz"),
    (".idx", ".vcf"),
    (".sa", ".fa"),
];

/// Words in input names and the extension of the files they suggest.
const WORDS: [(&str, &str); 14] = [
    ("fastq", ".fastq"),
    ("fq", ".fastq"),
    ("reads", ".fastq"),
    ("cram", ".cram"),
    ("bam", ".bam"),
    ("vcf", ".vcf"),
    ("fasta", ".fa"),
    ("genome", ".fa"),
    ("reference", ".fa"),
    ("bed", ".bed"),
    ("gtf", ".gtf"),
    ("gff", ".gff"),
    ("json", ".json"),
    ("csv", ".csv"),
];

/// A placeholder for an input parameter; `base` is the folder URL, ending in `/`, that files
/// are named in.
pub fn input(input: &InputParameter, types: &Types, base: &str) -> Value {
    let name = short_name(&input.id);
    let hint = hint_of(name, &input.format, &input.secondary_files);
    value(types, &input.value_type, name, base, &hint)
}

/// What a placeholder file of a parameter or record field looks like: its first format that
/// is no expression, and the extension that format, the secondary files or the name suggest.
pub fn hint_of(name: &str, formats: &[Expression], secondary: &[SecondaryFile]) -> Hint {
    let format = formats
        .iter()
        .find(|format| !has_code(format))
        .map(|format| format.text.clone());
    Hint {
        extension: extension(name, format.as_deref(), secondary),
        format,
    }
}

/// The extension a placeholder file gets: the one its format, its secondary files or its name
/// suggest, else `.txt`.
fn extension(name: &str, format: Option<&str>, secondary: &[SecondaryFile]) -> String {
    suggested(name, format, secondary)
        .unwrap_or(".txt")
        .to_owned()
}

/// The extension that the format, the secondary files or the name of a parameter suggest.
fn suggested(
    name: &str,
    format: Option<&str>,
    secondary: &[SecondaryFile],
) -> Option<&'static str> {
    if let Some(format) = format {
        let term = format.rsplit(['/', '#', ':']).next().unwrap_or(format);
        if let Some((_, extension)) = FORMATS.iter().find(|(known, _)| *known == term) {
            return Some(extension);
        }
    }
    for entry in secondary {
        let pattern = entry
            .pattern
            .text
            .trim_start_matches('^')
            .trim_end_matches('?');
        if let Some((_, extension)) = SECONDARY.iter().find(|(known, _)| *known == pattern) {
            return Some(extension);
        }
    }
    let name = name.to_lowercase();
    WORDS
        .iter()
        .find(|(word, _)| name.contains(word))
        .map(|(_, extension)| *extension)
}

/// The extension that an output suggests by its format, secondary files or name, if any.
pub fn suggested_extension(
    name: &str,
    format: Option<&Expression>,
    secondary: &[SecondaryFile],
) -> Option<&'static str> {
    let format = format.filter(|format| !has_code(format));
    suggested(name, format.map(|format| format.text.as_str()), secondary)
}

/// A value of a type for a parameter that nothing sets: a file named after the parameter in
/// the folder `base` (a URL ending in `/`), two items for arrays, the first symbol of enums.
pub fn value(types: &Types, value_type: &Type, name: &str, base: &str, hint: &Hint) -> Value {
    match types.resolve(value_type) {
        Type::Null => Value::Null,
        Type::Boolean => Value::Bool(false),
        Type::Int | Type::Long => Value::Integer(1),
        Type::Float | Type::Double => Value::Float(1.0),
        Type::String | Type::Any | Type::Named(_) => Value::string(name),
        Type::File | Type::Stdin | Type::Stdout | Type::Stderr => {
            let mut file = file_object("File", &format!("{name}{}", hint.extension), base);
            if let (Value::Map(map), Some(format)) = (&mut file, &hint.format) {
                map.insert("format".into(), Value::String(format.clone()));
            }
            file
        }
        Type::Directory => directory(name, base, true),
        // Two items, as expressions often look at the second one too, like paired reads.
        Type::Array(array) => Value::List(vec![
            value(types, &array.items, name, base, hint),
            value(types, &array.items, &format!("{name}_2"), base, hint),
        ]),
        Type::Record(record) => {
            let mut map = Map::new();
            for field in &record.fields {
                let hint = hint_of(&field.name, &field.format, &field.secondary_files);
                let value = value(types, &field.value_type, &field.name, base, &hint);
                map.insert(field.name.clone(), value);
            }
            Value::Map(map)
        }
        Type::Enum(schema) => schema.symbols.first().map_or(Value::Null, Value::string),
        Type::Union(members) => {
            // Files, as expressions over such unions mostly look at file fields.
            let file = members
                .iter()
                .find(|member| matches!(types.resolve(member), Type::File | Type::Directory));
            let first = members.iter().find(|member| !matches!(member, Type::Null));
            match file.or(first) {
                Some(member) => value(types, member, name, base, hint),
                None => Value::Null,
            }
        }
    }
}

/// A placeholder directory: at its top a folder, `<name>_1`, that holds a file, then a file,
/// since expressions look into the first entry, or take it as a file. `nested` makes the
/// folder at the top.
fn directory(name: &str, base: &str, nested: bool) -> Value {
    let mut directory = file_object("Directory", name, base);
    let folder = format!("{}/", directory.text("location").unwrap_or(base));
    let mut listing = Vec::new();
    if nested {
        listing.push(self::directory(&format!("{name}_1"), &folder, false));
    }
    listing.push(file_object("File", &format!("{name}.txt"), &folder));
    if let Value::Map(map) = &mut directory {
        map.insert("basename".into(), Value::string(name));
        map.insert("listing".into(), Value::List(listing));
    }
    directory
}

/// The strings that the placeholders of the inputs of a process hold, like the parameter
/// names; such a string can reach a parameter that it does not fit, like an enum, where a real
/// job gives a valid value.
pub fn strings(process: &Process, types: &Types, base: &str) -> HashSet<String> {
    fn collect(value: &Value, strings: &mut HashSet<String>) {
        match value {
            Value::String(text) => {
                strings.insert(text.clone());
            }
            Value::List(items) => items.iter().for_each(|item| collect(item, strings)),
            Value::Map(map) if !value.is_file_like() => {
                map.values().for_each(|item| collect(item, strings));
            }
            _ => {}
        }
    }
    let mut strings = HashSet::new();
    for parameter in &process.inputs {
        collect(&input(parameter, types, base), &mut strings);
    }
    strings
}

/// Whether a value is a string that placeholders hold, or a list of only such strings.
pub fn is_placeholder_string(value: &Value, strings: &HashSet<String>) -> bool {
    match value {
        Value::String(text) => strings.contains(text),
        Value::List(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| is_placeholder_string(item, strings))
        }
        _ => false,
    }
}

/// What a placeholder file looks like: the extension of its name and its format.
#[derive(Clone, Debug, Default)]
pub struct Hint {
    pub extension: String,
    pub format: Option<String>,
}

fn file_object(class: &str, name: &str, base: &str) -> Value {
    let mut map = Map::new();
    map.insert("class".into(), Value::string(class));
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    map.insert("location".into(), Value::String(format!("{base}{safe}")));
    Value::Map(map)
}

/// The input object with placeholders for the optional inputs that are null, have no default
/// and, when an error message is given, are named in it as `inputs.<name>`; `None` when there
/// are none.
pub fn fill_optional(
    process: &Process,
    job: &Map,
    outer: &Scope,
    base: &str,
    error: Option<&str>,
) -> Option<Map> {
    let scope = outer.enter(process);
    let mut types = scope.types();
    for input in &process.inputs {
        types.add(&input.value_type);
    }
    let mut filled = job.clone();
    let mut changed = false;
    for input in &process.inputs {
        let name = short_name(&input.id);
        let missing = filled.get(name).is_none_or(Value::is_null);
        let named = error.is_none_or(|error| names(error, name));
        if missing && named && input.default.is_none() && types.optional(&input.value_type) {
            let value = self::input(input, &types, base);
            filled.insert(name.to_owned(), value);
            changed = true;
        }
    }
    changed.then_some(filled)
}

/// Whether a message names an input as `inputs.<name>`, `inputs['<name>']` or
/// `inputs["<name>"]`.
pub fn names(message: &str, name: &str) -> bool {
    let references = [
        format!("inputs.{name}"),
        format!("inputs['{name}']"),
        format!("inputs[\"{name}\"]"),
    ];
    references.iter().any(|reference| {
        message.match_indices(reference.as_str()).any(|(at, _)| {
            let after = message[at + reference.len()..].chars().next();
            !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// Whether a value holds a file whose content is not real: one that a placeholder or a mock
/// task made, as its location starts with one of `prefixes`, or one that is not on disk, as
/// the data of a job file often is not in a mock run.
pub fn is_fake(value: &Value, prefixes: &[String]) -> bool {
    match value {
        Value::List(items) => items.iter().any(|item| is_fake(item, prefixes)),
        Value::Map(map) => {
            let location = map.get("location").and_then(Value::as_str);
            let absent = |location: &str| {
                crate::files::local_path(location).is_some_and(|path| !path.exists())
            };
            if value.is_file_like()
                && location.is_some_and(|location| {
                    prefixes.iter().any(|prefix| location.starts_with(prefix)) || absent(location)
                })
            {
                return true;
            }
            map.values().any(|item| is_fake(item, prefixes))
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "placeholder_tests.rs"]
mod tests;
