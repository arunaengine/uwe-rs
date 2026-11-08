//! The fields of `split`, which cuts text, CSV, FASTA, FASTQ and JSON items into records or
//! chunks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::closure::Body;

/// Splits each source item completely before the next one: `size` records per chunk, at
/// most `limit` records of each item, written as files with `files`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Split {
    pub format: SplitFormat,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// The element of a tuple item to split; by default its first File, else element zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elem: Option<u32>,
    /// The two elements of a tuple item that hold paired FASTQ files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair: Option<Pair>,
    #[serde(default, skip_serializing_if = "Decompress::is_auto")]
    pub decompress: Decompress,
    #[serde(default = "utf8", skip_serializing_if = "is_utf8")]
    pub charset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<ChunkFiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
}

/// The format of the items, with its own options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SplitFormat {
    Text(TextSplit),
    Csv(CsvSplit),
    Fasta(SequenceSplit),
    Fastq(SequenceSplit),
    Json(JsonSplit),
}

/// Lines; with `keep_header` the first line starts every chunk.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSplit {
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_header: bool,
}

/// CSV records. A null `quote` reads quotes as text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CsvSplit {
    #[serde(default = "comma", skip_serializing_if = "is_comma")]
    pub separator: char,
    #[serde(default = "double_quote", skip_serializing_if = "is_double_quote")]
    pub quote: Option<char>,
    #[serde(default, skip_serializing_if = "Header::is_none")]
    pub header: Header,
    #[serde(default, skip_serializing_if = "is_nothing")]
    pub skip: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strip: bool,
}

impl Default for CsvSplit {
    fn default() -> Self {
        CsvSplit {
            separator: ',',
            quote: Some('"'),
            header: Header::None,
            skip: 0,
            strip: false,
        }
    }
}

/// The column names of CSV records: none, read from the first record, or given.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Header {
    #[default]
    None,
    Read,
    Names(Vec<String>),
}

impl Header {
    pub fn is_none(&self) -> bool {
        *self == Header::None
    }
}

/// FASTA or FASTQ records: their text, or with `fields` records of the named fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceSplit {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<RecordField>,
}

/// One field of a sequence record, under a name of the user's choice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordField {
    pub name: String,
    pub field: SequenceField,
}

/// The fields of FASTA records (`id` to `sequence`) and of FASTQ records (`read_header` on).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceField {
    Id,
    Header,
    Desc,
    Text,
    Sequence,
    SeqString,
    ReadHeader,
    ReadString,
    QualityHeader,
    QualityString,
}

impl SequenceField {
    /// Whether FASTA records have the field; FASTQ records have the others and `text`.
    pub fn in_fasta(&self) -> bool {
        matches!(
            self,
            SequenceField::Id
                | SequenceField::Header
                | SequenceField::Desc
                | SequenceField::Text
                | SequenceField::Sequence
                | SequenceField::SeqString
        )
    }
}

/// The elements of a JSON document at `path`: dot-separated names and bracket indices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonSplit {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

/// Which two tuple elements hold paired files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Pair {
    /// The first two File elements.
    Auto,
    At([u32; 2]),
}

/// Whether the source files are gzip files: by name and content, always or never.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decompress {
    #[default]
    Auto,
    Always,
    Never,
}

impl Decompress {
    pub fn is_auto(&self) -> bool {
        *self == Decompress::Auto
    }
}

/// Chunks written as files of the run: named after `name` or the source, in `directory`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkFiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compress: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn one() -> u32 {
    1
}

fn is_one(value: &u32) -> bool {
    *value == 1
}

fn is_nothing(value: &u64) -> bool {
    *value == 0
}

fn utf8() -> String {
    "utf-8".into()
}

fn is_utf8(value: &str) -> bool {
    value == "utf-8"
}

fn comma() -> char {
    ','
}

fn is_comma(value: &char) -> bool {
    *value == ','
}

fn double_quote() -> Option<char> {
    Some('"')
}

fn is_double_quote(value: &Option<char>) -> bool {
    *value == Some('"')
}
