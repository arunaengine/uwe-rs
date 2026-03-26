//! Placeholder values for inputs that nothing sets, so that a graph builds without a job
//! file: files named after the input, with the extension that their format, their secondary
//! files or their name suggest, since expressions often take names apart.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{InputParameter, short_name};

use crate::inputs::has_code;
use crate::types::{Hint, Types};
use crate::value::Value;

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
/// are named in. Optional inputs get null when `null_if_optional` holds.
pub fn input(input: &InputParameter, types: &Types, base: &str, null_if_optional: bool) -> Value {
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
    types.placeholder(&input.value_type, name, base, null_if_optional, &hint)
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
