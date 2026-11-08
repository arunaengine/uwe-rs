//! The stream op `split`: each source item, a text or a File, or a tuple with one, is cut
//! into records or chunks by the splitter of the Nextflow evaluator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::split::{Format, Splitter};
use nextflow_evaluator::value::{PathValue, Value};
use serde_json::Value as Json;
use wir::reserved::{Header, Pair, SequenceField, Split, SplitFormat};

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::groovy;
use crate::operator::{Operator, Pull, Step, Turn};
use crate::streams::Failure;

/// Splits the items of `source`, one item a step.
pub struct Splitting {
    pub spec: Split,
    pub callbacks: Callbacks,
}

impl Splitting {
    pub fn new(spec: Split) -> Splitting {
        Splitting {
            spec,
            callbacks: Callbacks::default(),
        }
    }

    /// The splitter of the format and sizes of the op.
    fn splitter(&self) -> Splitter {
        let format = match &self.spec.format {
            SplitFormat::Text(_) => Format::Text { keep_header: false },
            SplitFormat::Csv(csv) => Format::Csv {
                separator: csv.separator,
                quote: csv.quote,
                header: match &csv.header {
                    Header::None => None,
                    Header::Read => Some(None),
                    Header::Names(names) => Some(Some(
                        names
                            .iter()
                            .map(|name| Value::string(name.as_str()))
                            .collect(),
                    )),
                },
                skip: csv.skip as usize,
                strip: csv.strip,
            },
            SplitFormat::Fasta(sequence) => Format::Fasta(record(&sequence.fields)),
            SplitFormat::Fastq(sequence) => Format::Fastq(record(&sequence.fields)),
            SplitFormat::Json(_) => Format::Text { keep_header: false },
        };
        // One record a chunk is the record itself, more are an array or a joined text.
        let by = (self.spec.size > 1).then_some(self.spec.size as usize);
        Splitter {
            format,
            by,
            limit: self.spec.limit.map(|limit| limit as usize),
            simulated: false,
        }
    }

    /// The text of a text item, or of the local file of a File item.
    fn text(&self, turn: &Turn<'_>, item: &Json) -> Result<String, Failure> {
        match item {
            Json::String(text) => Ok(text.replace("\r\n", "\n")),
            Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let path = location.strip_prefix("file://").unwrap_or(location);
                if path.contains("://") {
                    return Err(
                        turn.failed(format!("uwe splits only local files yet, not {location}"))
                    );
                }
                let bytes = std::fs::read(path)
                    .map_err(|error| turn.failed(format!("cannot read {path}: {error}")))?;
                let gzip = bytes.starts_with(&[0x1f, 0x8b]) || path.ends_with(".gz");
                if gzip && self.spec.decompress != wir::reserved::Decompress::Never {
                    return Err(turn.failed(format!("uwe does not decompress {path} yet")));
                }
                decode(&bytes, &self.spec.charset)
                    .map(|text| text.replace("\r\n", "\n"))
                    .map_err(|why| turn.failed(format!("{path}: {why}")))
            }
            other => Err(turn.failed(format!("cannot split {other}"))),
        }
    }

    /// The chunks of one source item, with the item rebuilt around each of them; paired
    /// FASTQ files give aligned chunks of both.
    fn chunks(&self, turn: &Turn<'_>, item: &Json) -> Result<Vec<Json>, Failure> {
        let Json::Array(fields) = item else {
            return self.pieces(turn, item);
        };
        let files: Vec<usize> = (fields.iter().enumerate())
            .filter(|(_, field)| field.get("class").and_then(Json::as_str) == Some("File"))
            .map(|(at, _)| at)
            .collect();
        let places = match &self.spec.pair {
            Some(Pair::At([first, second])) => vec![*first as usize, *second as usize],
            Some(Pair::Auto) if files.len() >= 2 => files[..2].to_vec(),
            Some(Pair::Auto) => return Err(turn.failed("a paired split needs two File elements")),
            None => vec![
                self.spec
                    .elem
                    .map(|elem| elem as usize)
                    .or(files.first().copied())
                    .unwrap_or(0),
            ],
        };
        let mut columns = Vec::new();
        for place in &places {
            let source = fields
                .get(*place)
                .ok_or_else(|| turn.failed(format!("the item has no element {place}")))?;
            columns.push(self.pieces(turn, source)?);
        }
        if columns
            .iter()
            .any(|column| column.len() != columns[0].len())
        {
            return Err(turn.failed("the paired files have different numbers of records"));
        }
        Ok((0..columns[0].len())
            .map(|at| {
                let mut fields = fields.clone();
                for (place, column) in places.iter().zip(&columns) {
                    fields[*place] = column[at].clone();
                }
                Json::Array(fields)
            })
            .collect())
    }

    /// The chunks of one text or File.
    fn pieces(&self, turn: &Turn<'_>, source: &Json) -> Result<Vec<Json>, Failure> {
        let mut text = self.text(turn, source)?;
        // Text records end with a newline, also the last one.
        let lines = !matches!(self.spec.format, SplitFormat::Csv(_) | SplitFormat::Json(_));
        if lines && !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        // A kept header leaves the records and starts every chunk.
        let header = match &self.spec.format {
            SplitFormat::Text(options) if options.keep_header => {
                let end = text.find('\n').map_or(text.len(), |at| at + 1);
                Some(text.drain(..end).collect::<String>())
            }
            _ => None,
        };
        let chunks = self
            .splitter()
            .text(text)
            .map_err(|error| turn.failed(error.to_string()))?;
        let mut pieces = Vec::new();
        for chunk in chunks {
            let chunk = chunk.map_err(|error| turn.failed(error.to_string()))?;
            let value = self.named(groovy::to_json(&chunk).map_err(|why| turn.failed(why))?);
            pieces.push(match (&header, value) {
                (Some(header), Json::String(text)) => Json::String(format!("{header}{text}")),
                (_, value) => value,
            });
        }
        Ok(pieces)
    }

    /// A sequence record, or an array of them, with the field names of the op.
    fn named(&self, value: Json) -> Json {
        let fields = match &self.spec.format {
            SplitFormat::Fasta(sequence) | SplitFormat::Fastq(sequence) => &sequence.fields,
            _ => return value,
        };
        match value {
            Json::Array(items) => {
                Json::Array(items.into_iter().map(|item| self.named(item)).collect())
            }
            Json::Object(mut record) => Json::Object(
                (fields.iter())
                    .map(|field| {
                        (
                            field.name.clone(),
                            record.remove(native(field.field)).unwrap_or_default(),
                        )
                    })
                    .collect(),
            ),
            other => other,
        }
    }
}

/// The name of a sequence field in the records of the Nextflow splitter.
fn native(field: SequenceField) -> &'static str {
    match field {
        SequenceField::Id => "id",
        SequenceField::Header => "header",
        SequenceField::Desc => "desc",
        SequenceField::Text => "text",
        SequenceField::Sequence => "sequence",
        SequenceField::SeqString => "seqString",
        SequenceField::ReadHeader => "readHeader",
        SequenceField::ReadString => "readString",
        SequenceField::QualityHeader => "qualityHeader",
        SequenceField::QualityString => "qualityString",
    }
}

impl Operator for Splitting {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let item = match turn.pull("source")? {
            Pull::Item(item) => item,
            Pull::Pending => return Ok(Step::Idle),
            Pull::Closed => return Ok(Step::Done),
        };
        for chunk in self.chunks(turn, &item)? {
            let value = match &self.spec.body {
                Some(body) => {
                    let body = body.clone();
                    (self
                        .callbacks
                        .call(context, &body, (&["item"], &[chunk]), &turn.values))
                    .map_err(|why| turn.failed(why))?
                }
                None => chunk,
            };
            turn.emit("out", value);
        }
        Ok(Step::Moved)
    }
}

/// The native names of the fields of sequence records, or none for their text.
fn record(fields: &[wir::reserved::RecordField]) -> Option<Vec<String>> {
    (!fields.is_empty()).then(|| {
        fields
            .iter()
            .map(|field| native(field.field).to_owned())
            .collect()
    })
}

/// The text of bytes in a charset that a split reads.
fn decode(bytes: &[u8], charset: &str) -> Result<String, String> {
    match charset.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => {
            String::from_utf8(bytes.to_vec()).map_err(|_| "the text is no UTF-8".into())
        }
        "us-ascii" | "ascii" if bytes.is_ascii() => {
            Ok(bytes.iter().map(|byte| char::from(*byte)).collect())
        }
        "us-ascii" | "ascii" => Err("the text is no ASCII".into()),
        "iso-8859-1" | "latin-1" | "latin1" => {
            Ok(bytes.iter().map(|byte| char::from(*byte)).collect())
        }
        other => Err(format!("uwe does not read the charset {other}")),
    }
}

/// A path value of a location, for the splitter of files.
#[allow(dead_code, reason = "the file splitters of the next commits use it")]
pub fn path_of(location: &str) -> Rc<PathValue> {
    Rc::new(PathValue::parse(location, "/"))
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{commit, node, steps, wired};
    use crate::operator::{Step, make};

    /// What a split with `fields` gives for the items of a closed stream, or why it failed.
    pub fn split(fields: Json, items: &[Json]) -> Result<Vec<Json>, String> {
        let node = node(json!({"split": fields}), &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, &["source"]);
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        match step {
            Ok(Step::Done) => Ok(emitted),
            Ok(other) => panic!("the split did not end: {other:?}"),
            Err(failure) => Err(failure.message),
        }
    }

    #[test]
    fn split_text() {
        let text = json!("a\r\nb\nc");
        assert_eq!(
            split(json!({"format": {"text": {}}}), std::slice::from_ref(&text)),
            Ok(vec![json!("a\n"), json!("b\n"), json!("c\n")])
        );
        let chunks = split(
            json!({"format": {"text": {}}, "size": 2}),
            std::slice::from_ref(&text),
        );
        assert_eq!(
            chunks,
            Ok(vec![json!("a\nb\n"), json!("c\n")]),
            "the last chunk is partial"
        );
        let header = split(
            json!({"format": {"text": {"keep_header": true}}, "size": 1}),
            &[json!("h\n1\n2\n")],
        );
        assert_eq!(
            header,
            Ok(vec![json!("h\n1\n"), json!("h\n2\n")]),
            "the header starts every chunk"
        );
        let limited = split(
            json!({"format": {"text": {}}, "limit": 1}),
            &[text.clone(), json!("x\ny")],
        );
        assert_eq!(
            limited,
            Ok(vec![json!("a\n"), json!("x\n")]),
            "a limit counts the records of each item"
        );
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("in.txt");
        std::fs::write(&path, b"caf\xe9\n").unwrap();
        let file = json!({"class": "File", "location": path.to_string_lossy()});
        let latin = split(
            json!({"format": {"text": {}}, "charset": "latin-1"}),
            std::slice::from_ref(&file),
        );
        assert_eq!(latin, Ok(vec![json!("café\n")]));
        assert!(
            split(json!({"format": {"text": {}}}), &[file])
                .unwrap_err()
                .contains("no UTF-8")
        );
    }

    #[test]
    fn split_csv() {
        let text = json!("id,name\n1,\"a, \"\"b\"\"\"\n2, c \n");
        let rows = split(
            json!({"format": {"csv": {"header": "read", "strip": true}}}),
            std::slice::from_ref(&text),
        );
        assert_eq!(
            rows,
            Ok(vec![
                json!({"id": "1", "name": "a, \"b\""}),
                json!({"id": "2", "name": "c"})
            ])
        );
        let plain = split(json!({"format": {"csv": {"skip": 1}}, "size": 2}), &[text]);
        assert_eq!(
            plain,
            Ok(vec![json!([["1", "a, \"b\""], ["2", " c "]])]),
            "arrays of strings without a header"
        );
        let quoted = split(
            json!({"format": {"csv": {"separator": ";", "quote": null}}}),
            &[json!("\"x\";2\n")],
        );
        assert_eq!(
            quoted,
            Ok(vec![json!(["\"x\"", "2"])]),
            "a null quote reads quotes as text"
        );
    }

    #[test]
    fn split_tuple() {
        let file = |name: &str| json!({"class": "File", "location": format!("/none/{name}")});
        let tuple = json!(["s1", "a\nb\n", 7]);
        let chunks = split(json!({"format": {"text": {}}, "elem": 1}), &[tuple]);
        assert_eq!(
            chunks,
            Ok(vec![json!(["s1", "a\n", 7]), json!(["s1", "b\n", 7])]),
            "the other elements stay"
        );
        let missing = split(
            json!({"format": {"text": {}}}),
            &[json!(["s1", file("x.txt")])],
        );
        assert!(
            missing.unwrap_err().contains("cannot read /none/x.txt"),
            "the first File by default"
        );
    }
}

#[cfg(test)]
mod sequences {
    use serde_json::json;

    use super::tests::split;

    const FASTA: &str = ">s1 first one\nACGT\nAC\n>s2\nGG\n";
    const FASTQ: &str = "@r1\nACGT\n+\nIIII\n@r2\nGG\n+\nII\n";

    #[test]
    fn split_sequences() {
        let text = split(json!({"format": {"fasta": {}}}), &[json!(FASTA)]);
        assert_eq!(
            text,
            Ok(vec![json!(">s1 first one\nACGT\nAC\n"), json!(">s2\nGG\n")])
        );
        let fields = json!({"fasta": {"fields": [{"name": "name", "field": "id"}, {"name": "seq", "field": "seq_string"}]}});
        let records = split(json!({"format": fields}), &[json!(FASTA)]);
        assert_eq!(
            records,
            Ok(vec![
                json!({"name": "s1", "seq": "ACGTAC"}),
                json!({"name": "s2", "seq": "GG"})
            ])
        );
        let reads = json!({"fastq": {"fields": [{"name": "readString", "field": "read_string"}, {"name": "q", "field": "quality_string"}]}});
        let chunked = split(json!({"format": reads, "size": 2}), &[json!(FASTQ)]);
        assert_eq!(
            chunked,
            Ok(vec![
                json!([{"readString": "ACGT", "q": "IIII"}, {"readString": "GG", "q": "II"}])
            ])
        );
    }

    #[test]
    fn split_pairs() {
        let folder = tempfile::tempdir().unwrap();
        let file = |name: &str, text: &str| {
            let path = folder.path().join(name);
            std::fs::write(&path, text).unwrap();
            json!({"class": "File", "location": path.to_string_lossy()})
        };
        let (one, two) = (file("r1.fq", FASTQ), file("r2.fq", FASTQ));
        let pairs = split(
            json!({"format": {"fastq": {}}, "pair": "auto"}),
            &[json!(["s", one, two])],
        );
        let first = pairs.unwrap().remove(0);
        assert_eq!(
            first,
            json!(["s", "@r1\nACGT\n+\nIIII\n", "@r1\nACGT\n+\nIIII\n"]),
            "aligned chunks of both files"
        );
        let short = file("r3.fq", "@r1\nA\n+\nI\n");
        let unequal = split(
            json!({"format": {"fastq": {}}, "pair": {"at": [1, 2]}}),
            &[json!(["s", file("r4.fq", FASTQ), short])],
        );
        assert!(
            unequal
                .unwrap_err()
                .contains("different numbers of records")
        );
    }
}
