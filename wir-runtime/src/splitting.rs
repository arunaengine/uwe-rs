//! The stream op `split`: each source item, a text or a File, or a tuple with one, is cut
//! into records or chunks by the splitter of the Nextflow evaluator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use nextflow_evaluator::split::{Format, Splitter};
use nextflow_evaluator::value::Value;
use serde_json::Value as Json;
use wir::reserved::{ChunkFiles, Decompress, Header, Pair, SequenceField, Split, SplitFormat};

use crate::callback::Callbacks;
use crate::engine::Context;
use crate::groovy;
use crate::operator::{Operator, Pull, Step, Turn};
use crate::streams::Failure;

/// Splits the items of `source`, one item a step.
pub struct Splitting {
    pub spec: Split,
    pub callbacks: Callbacks,
    /// The number of source items split so far.
    pub sources: u64,
}

impl Splitting {
    pub fn new(spec: Split) -> Splitting {
        Splitting {
            spec,
            callbacks: Callbacks::default(),
            sources: 0,
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
            SplitFormat::Fastq(sequence) => {
                Format::Fastq(record(&sequence.fields).map(|mut names| {
                    // The splitter gives no FASTQ text; `named` makes it of the four lines.
                    if names.iter().any(|name| name == "text") {
                        names.extend(FASTQ_LINES.map(str::to_owned));
                    }
                    names
                }))
            }
            SplitFormat::Json(json) => Format::Json {
                path: json.path.clone(),
            },
        };
        // One record a chunk is the record itself, more are an array or a joined text; the
        // splitter joins JSON records as text, so `pieces` groups them.
        let json = matches!(self.spec.format, SplitFormat::Json(_));
        let by = (self.spec.size > 1 && !json).then_some(self.spec.size as usize);
        Splitter {
            format,
            by,
            limit: self.spec.limit.map(|limit| limit as usize),
            simulated: false,
        }
    }

    /// The text of a text item, or of the local file of a File item.
    fn text(&self, turn: &Turn<'_>, context: &Context<'_>, item: &Json) -> Result<String, Failure> {
        match item {
            Json::String(text) => Ok(text.replace("\r\n", "\n")),
            Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let path = location.strip_prefix("file://").unwrap_or(location);
                if path.contains("://") && context.provider.is_none() {
                    return Err(
                        turn.failed(format!("uwe splits only local files yet, not {location}"))
                    );
                }
                let bytes = (context.read(location))
                    .map_err(|error| turn.failed(format!("cannot read {path}: {error}")))?;
                let gzip = match self.spec.decompress {
                    Decompress::Always => true,
                    Decompress::Never => false,
                    Decompress::Auto => bytes.starts_with(&[0x1f, 0x8b]) || path.ends_with(".gz"),
                };
                let bytes = match gzip {
                    true => {
                        let mut plain = Vec::new();
                        let mut reader = flate2::read::MultiGzDecoder::new(bytes.as_slice());
                        std::io::Read::read_to_end(&mut reader, &mut plain).map_err(|error| {
                            turn.failed(format!("cannot decompress {path}: {error}"))
                        })?;
                        plain
                    }
                    false => bytes,
                };
                decode(&bytes, &self.spec.charset)
                    .map(|text| text.replace("\r\n", "\n"))
                    .map_err(|why| turn.failed(format!("{path}: {why}")))
            }
            other => Err(turn.failed(format!("cannot split {other}"))),
        }
    }

    /// The chunks of one source item, with the item rebuilt around each of them; paired
    /// FASTQ files give aligned chunks of both.
    fn chunks(
        &self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        item: &Json,
    ) -> Result<Vec<Json>, Failure> {
        let Json::Array(fields) = item else {
            return Ok(self.pieces(turn, context, item, None)?.0);
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
        let (mut columns, mut records) = (Vec::new(), Vec::new());
        for (mate, place) in places.iter().enumerate() {
            let source = fields
                .get(*place)
                .ok_or_else(|| turn.failed(format!("the item has no element {place}")))?;
            let mate = (places.len() > 1).then_some(mate + 1);
            let (column, count) = self.pieces(turn, context, source, mate)?;
            columns.push(column);
            records.push(count);
        }
        if records.iter().any(|count| *count != records[0]) {
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

    /// The chunks of one text or File, of the mate `mate` of a pair, and how many records
    /// they hold.
    fn pieces(
        &self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        source: &Json,
        mate: Option<usize>,
    ) -> Result<(Vec<Json>, usize), Failure> {
        let mut text = self.text(turn, context, source)?;
        if let SplitFormat::Json(json) = &self.spec.format {
            let document: Json = serde_json::from_str(&text)
                .map_err(|error| turn.failed(format!("the JSON does not parse: {error}")))?;
            if !json.path.is_empty() && reach(&document, &json.path).is_none() {
                return Err(turn.failed(format!("the JSON path {} is missing", json.path)));
            }
        }
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
        let mut chunks = self
            .splitter()
            .text(text)
            .map_err(|error| turn.failed(error.to_string()))?;
        let mut pieces = Vec::new();
        for chunk in chunks.by_ref() {
            let chunk = chunk.map_err(|error| turn.failed(error.to_string()))?;
            let value = self.named(groovy::to_json(&chunk).map_err(|why| turn.failed(why))?);
            pieces.push(match (&header, value) {
                (Some(header), Json::String(text)) => Json::String(format!("{header}{text}")),
                (_, value) => value,
            });
        }
        if matches!(self.spec.format, SplitFormat::Json(_)) && self.spec.size > 1 {
            let size = self.spec.size as usize;
            pieces = (pieces.chunks(size).map(|group| Json::Array(group.to_vec()))).collect();
        }
        let pieces = match &self.spec.files {
            Some(files) => self.written(turn, context, (files, mate), source, pieces)?,
            None => pieces,
        };
        Ok((pieces, chunks.records()))
    }

    /// The text chunks of a source written as files of the op in the file area of the run,
    /// all before any is given: `<name or stem>.<source>.<chunk>`, with `_<mate>` for a pair,
    /// and the format extension.
    fn written(
        &self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        (files, mate): (&ChunkFiles, Option<usize>),
        source: &Json,
        pieces: Vec<Json>,
    ) -> Result<Vec<Json>, Failure> {
        let mut folder = context.files.join(&turn.path);
        match files.directory.as_deref() {
            Some(directory) if directory.contains("://") => {
                return Err(turn.failed(format!(
                    "uwe writes chunk files only to local folders yet, not {directory}"
                )));
            }
            Some(directory) if directory.starts_with('/') => folder = PathBuf::from(directory),
            Some(directory) if directory.split('/').any(|part| part == "..") => {
                return Err(turn.failed(format!("the folder {directory} leaves the file area")));
            }
            Some(directory) => folder = folder.join(directory),
            None => {}
        }
        let basename = source
            .get("basename")
            .and_then(Json::as_str)
            .unwrap_or("chunk");
        if let Some(name) = [Some(basename), files.name.as_deref()]
            .into_iter()
            .flatten()
            .find(|name| !plain(name))
        {
            return Err(turn.failed(format!("the file name {name:?} leaves its folder")));
        }
        std::fs::create_dir_all(&folder)
            .map_err(|error| turn.failed(format!("cannot make {}: {error}", folder.display())))?;
        let stem = basename.trim_end_matches(".gz");
        let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem);
        let stem = files.name.as_deref().unwrap_or(stem);
        let extension = match self.spec.format {
            SplitFormat::Fasta(_) => "fa",
            SplitFormat::Fastq(_) => "fq",
            _ => "txt",
        };
        let mut written = Vec::new();
        for (chunk, piece) in pieces.into_iter().enumerate() {
            let Json::String(text) = piece else {
                return Err(turn.failed("only text chunks are written as files"));
            };
            let gzip = if files.compress { ".gz" } else { "" };
            let mate = mate.map_or(String::new(), |mate| format!("_{mate}"));
            let name = format!("{stem}.{}.{chunk}{mate}.{extension}{gzip}", self.sources);
            let path = folder.join(&name);
            if path.parent() != Some(folder.as_path()) {
                return Err(turn.failed(format!("the chunk {name:?} leaves its folder")));
            }
            let bytes = match files.compress {
                true => {
                    let mut encoder = flate2::GzBuilder::new()
                        .mtime(0)
                        .write(Vec::new(), flate2::Compression::default());
                    std::io::Write::write_all(&mut encoder, text.as_bytes())
                        .and_then(|()| encoder.finish())
                }
                false => Ok(text.into_bytes()),
            };
            let size = (bytes
                .and_then(|bytes| std::fs::write(&path, &bytes).map(|()| bytes.len())))
            .map_err(|error| turn.failed(format!("cannot write {}: {error}", path.display())))?;
            written.push(serde_json::json!({"class": "File", "location": path.to_string_lossy(), "basename": name, "size": size}));
        }
        Ok(written)
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
            Json::Object(mut record) => {
                let text = fields
                    .iter()
                    .any(|field| field.field == SequenceField::Text);
                if text && matches!(self.spec.format, SplitFormat::Fastq(_)) {
                    let line = |name| record.get(name).and_then(Json::as_str).unwrap_or_default();
                    let [header, read, quality, string] = FASTQ_LINES.map(line);
                    let text = format!("@{header}\n{read}\n+{quality}\n{string}\n");
                    record.insert("text".into(), Json::String(text));
                }
                Json::Object(
                    (fields.iter())
                        .map(|field| {
                            (
                                field.name.clone(),
                                record.remove(native(field.field)).unwrap_or_default(),
                            )
                        })
                        .collect(),
                )
            }
            other => other,
        }
    }
}

/// The fields of the four lines of a FASTQ record in the records of the Nextflow splitter.
const FASTQ_LINES: [&str; 4] = ["readHeader", "readString", "qualityHeader", "qualityString"];

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
        let chunks = self.chunks(turn, context, &item)?;
        self.sources += 1;
        for chunk in chunks {
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

/// The value at a JSON path of member names and bracket indices, if there is one.
fn reach<'v>(document: &'v Json, path: &str) -> Option<&'v Json> {
    let mut value = document;
    for part in path.split('.') {
        let (name, rest) = part.split_at(part.find('[').unwrap_or(part.len()));
        if !name.is_empty() {
            value = value.get(name)?;
        }
        for index in rest.split(['[', ']']).filter(|index| !index.is_empty()) {
            value = value.get(index.parse::<usize>().ok()?)?;
        }
    }
    Some(value)
}

/// Whether a file name names a file in its folder: not empty, no folder and no `..`.
fn plain(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
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

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{Fixture, Store, commit, node, steps_in, wired};
    use crate::operator::{Step, make};

    /// What a split with `fields` gives for the items of a closed stream, or why it failed.
    pub fn split(fields: Json, items: &[Json]) -> Result<Vec<Json>, String> {
        split_in(fields, items, &Fixture::default())
    }

    /// What [`split`] gives with the file area and files provider of `fixture`.
    pub fn split_in(fields: Json, items: &[Json], fixture: &Fixture) -> Result<Vec<Json>, String> {
        let node = node(json!({"split": fields}), &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, &["source"]);
        let context = fixture.context();
        let (emitted, _, step) = steps_in(
            operator.as_mut(),
            &node,
            &mut streams,
            &Map::new(),
            &context,
        );
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
        let fixture = Fixture {
            provider: Some(std::sync::Arc::new(Store(folder.path().to_owned()))),
            ..Fixture::default()
        };
        std::fs::write(folder.path().join("stored.txt"), "s\n").unwrap();
        let stored = json!({"class": "File", "location": "store://stored.txt"});
        assert_eq!(
            split_in(json!({"format": {"text": {}}}), &[stored], &fixture),
            Ok(vec![json!("s\n")]),
            "through the files provider of the run"
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
        let texts = json!({"fastq": {"fields": [{"name": "t", "field": "text"}, {"name": "id", "field": "read_header"}]}});
        assert_eq!(
            split(json!({"format": texts}), &[json!(FASTQ)]),
            Ok(vec![
                json!({"t": "@r1\nACGT\n+\nIIII\n", "id": "r1"}),
                json!({"t": "@r2\nGG\n+\nII\n", "id": "r2"})
            ]),
            "the text of a FASTQ record"
        );
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
        let out = folder.path().join("chunks");
        let unnamed = |name: &str, text: &str| {
            let mut file = file(name, text);
            file.as_object_mut().unwrap().remove("basename");
            file
        };
        let (one, two) = (
            unnamed("m1.fq", "@a\nA\n+\nI\n"),
            unnamed("m2.fq", "@b\nC\n+\nI\n"),
        );
        let files = json!({"format": {"fastq": {}}, "pair": "auto", "files": {"directory": out.to_string_lossy()}});
        let written = split(files, &[json!(["s", one, two])]).unwrap().remove(0);
        let read =
            |at: usize| std::fs::read_to_string(written[at]["location"].as_str().unwrap()).unwrap();
        assert_eq!(
            (read(1), read(2)),
            ("@a\nA\n+\nI\n".to_owned(), "@b\nC\n+\nI\n".to_owned()),
            "each mate keeps its own chunk file"
        );
        assert_eq!(written[2]["basename"], "chunk.0.0_2.fq");
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
        let three = FASTQ.to_owned() + "@r3\nT\n+\nI\n";
        let four = three.clone() + "@r4\nT\n+\nI\n";
        let chunked = split(
            json!({"format": {"fastq": {}}, "pair": "auto", "size": 2}),
            &[json!(["s", file("r5.fq", &three), file("r6.fq", &four)])],
        );
        assert!(
            chunked
                .unwrap_err()
                .contains("different numbers of records"),
            "records count, not chunks"
        );
    }
}

#[cfg(test)]
mod files {
    use std::io::Write;

    use serde_json::{Value as Json, json};

    use super::tests::split;

    #[test]
    fn split_json() {
        let document = json!("{\"b\": 1, \"a\": [null, {\"x\": 2}]}");
        let entries = split(
            json!({"format": {"json": {}}}),
            std::slice::from_ref(&document),
        );
        assert_eq!(
            entries,
            Ok(vec![
                json!({"key": "b", "value": 1}),
                json!({"key": "a", "value": [null, {"x": 2}]})
            ]),
            "entries in document order"
        );
        let items = split(
            json!({"format": {"json": {"path": "a"}}}),
            std::slice::from_ref(&document),
        );
        assert_eq!(
            items,
            Ok(vec![Json::Null, json!({"x": 2})]),
            "null elements stay"
        );
        let grouped = split(
            json!({"format": {"json": {}}, "size": 2}),
            &[json!("[1,2,3]")],
        );
        assert_eq!(
            grouped,
            Ok(vec![json!([1, 2]), json!([3])]),
            "records grouped into arrays"
        );
        let missing = split(json!({"format": {"json": {"path": "a[3]"}}}), &[document]);
        assert!(
            missing
                .unwrap_err()
                .contains("the JSON path a[3] is missing")
        );
    }

    #[test]
    fn split_gzip() {
        let folder = tempfile::tempdir().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"a\nb\n").unwrap();
        let bytes = encoder.finish().unwrap();
        let file = |name: &str| {
            let path = folder.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            json!({"class": "File", "location": path.to_string_lossy(), "basename": name})
        };
        let (named, plain) = (file("in.txt.gz"), file("in.bin"));
        let lines = Ok(vec![json!("a\n"), json!("b\n")]);
        assert_eq!(
            split(
                json!({"format": {"text": {}}}),
                std::slice::from_ref(&named)
            ),
            lines,
            "by name"
        );
        assert_eq!(
            split(
                json!({"format": {"text": {}}}),
                std::slice::from_ref(&plain)
            ),
            lines,
            "by content"
        );
        assert_eq!(
            split(
                json!({"format": {"text": {}}, "decompress": "always"}),
                std::slice::from_ref(&plain)
            ),
            lines
        );
        assert!(
            split(
                json!({"format": {"text": {}}, "decompress": "never"}),
                &[plain]
            )
            .unwrap_err()
            .contains("no UTF-8")
        );
    }

    #[test]
    fn split_files() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("reads.fq");
        std::fs::write(&source, "@r1\nA\n+\nI\n@r2\nC\n+\nI\n@r3\nG\n+\nI\n").unwrap();
        let file =
            json!({"class": "File", "location": source.to_string_lossy(), "basename": "reads.fq"});
        let out = folder.path().join("chunks");
        let fields = |compress: bool| json!({"format": {"fastq": {}}, "size": 2, "files": {"directory": out.to_string_lossy(), "compress": compress}});
        let chunks = split(fields(false), &[file.clone(), file.clone()]).unwrap();
        let names: Vec<&str> = chunks
            .iter()
            .map(|chunk| chunk["basename"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "reads.0.0.fq",
                "reads.0.1.fq",
                "reads.1.0.fq",
                "reads.1.1.fq"
            ],
            "stable names by source and chunk"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("reads.0.1.fq")).unwrap(),
            "@r3\nG\n+\nI\n"
        );
        let first = split(fields(true), std::slice::from_ref(&file)).unwrap();
        let again = split(fields(true), &[file]).unwrap();
        let read = |chunk: &Json| std::fs::read(chunk["location"].as_str().unwrap()).unwrap();
        assert_eq!(
            read(&first[0]),
            read(&again[0]),
            "gzip output does not depend on the time"
        );
        let blocked = folder.path().join("blocked");
        let escaping = |basename: &str| {
            let file = json!({"class": "File", "location": source.to_string_lossy(), "basename": basename});
            split(fields(false), &[file])
        };
        let outside = folder.path().join("x.fq");
        for basename in ["../x.fq", &*outside.to_string_lossy()] {
            let failed = escaping(basename).unwrap_err();
            assert!(failed.contains("leaves its folder"), "{failed}");
        }
        assert!(
            !folder.path().join("x.0.0.fq").exists() && !outside.with_extension("0.0.fq").exists(),
            "no chunk outside its folder"
        );
        std::fs::write(&blocked, "").unwrap();
        let fields = json!({"format": {"text": {}}, "files": {"directory": blocked.join("x").to_string_lossy()}});
        assert!(
            split(fields, &[json!("a\n")])
                .unwrap_err()
                .contains("cannot make"),
            "a failed write fails the op"
        );
    }
}
