//! Placeholder values for inputs that nothing sets, so that a graph builds without a job
//! file: files named after the input, with the extension that their format, their secondary
//! files or their name suggest, since expressions often take names apart. Every rule about
//! placeholders lives here: the values themselves, which optional inputs get them, and which
//! files count as placeholders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{InputParameter, Process, Type, short_name};

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
    let format = input
        .format
        .iter()
        .find(|format| !has_code(format))
        .map(|format| format.text.clone());
    let hint = Hint {
        extension: extension(input, format.as_deref()),
        format,
    };
    value(types, &input.value_type, name, base, &hint)
}

/// The extension a placeholder file of an input gets.
fn extension(input: &InputParameter, format: Option<&str>) -> String {
    if let Some(format) = format {
        let term = format.rsplit(['/', '#', ':']).next().unwrap_or(format);
        if let Some((_, extension)) = FORMATS.iter().find(|(known, _)| *known == term) {
            return (*extension).to_owned();
        }
    }
    for secondary in &input.secondary_files {
        let pattern = secondary
            .pattern
            .text
            .trim_start_matches('^')
            .trim_end_matches('?');
        if let Some((_, extension)) = SECONDARY.iter().find(|(known, _)| *known == pattern) {
            return (*extension).to_owned();
        }
    }
    let name = short_name(&input.id).to_lowercase();
    WORDS
        .iter()
        .find(|(word, _)| name.contains(word))
        .map_or(".txt", |(_, extension)| extension)
        .to_owned()
}

/// A value of a type for a parameter that nothing sets: a file named after the parameter in
/// the folder `base` (a URL ending in `/`), one item for arrays, the first symbol of enums.
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
        Type::Directory => {
            // One file inside, as expressions often look at the first entry.
            let mut directory = file_object("Directory", name, base);
            let folder = format!("{}/", directory.text("location").unwrap_or(base));
            let inside = file_object("File", &format!("{name}.txt"), &folder);
            if let Value::Map(map) = &mut directory {
                map.insert("basename".into(), Value::string(name));
                map.insert("listing".into(), Value::List(vec![inside]));
            }
            directory
        }
        Type::Array(array) => Value::List(vec![value(types, &array.items, name, base, hint)]),
        Type::Record(record) => {
            let mut map = Map::new();
            for field in &record.fields {
                let value = value(
                    types,
                    &field.value_type,
                    &field.name,
                    base,
                    &Hint::default(),
                );
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

/// Whether a value holds a file that a placeholder or a mock task made, whose content is
/// not real: its location starts with one of `prefixes`.
pub fn is_fake(value: &Value, prefixes: &[String]) -> bool {
    match value {
        Value::List(items) => items.iter().any(|item| is_fake(item, prefixes)),
        Value::Map(map) => {
            let location = map.get("location").and_then(Value::as_str);
            if value.is_file_like()
                && location.is_some_and(|location| {
                    prefixes.iter().any(|prefix| location.starts_with(prefix))
                })
            {
                return true;
            }
            map.values().any(|item| is_fake(item, prefixes))
        }
        _ => false,
    }
}
