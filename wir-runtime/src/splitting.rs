//! The stream op `split`: each source item, a text or a File, or a tuple with one, is cut
//! into records or chunks by the splitter of the Nextflow evaluator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::split::{Format, Splitter};
use nextflow_evaluator::value::{PathValue, Value};
use serde_json::Value as Json;
use wir::reserved::{Header, Split, SplitFormat};

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
            SplitFormat::Fasta(_) | SplitFormat::Fastq(_) | SplitFormat::Json(_) => {
                Format::Text { keep_header: false }
            }
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
                decode(&bytes, &self.spec.charset)
                    .map(|text| text.replace("\r\n", "\n"))
                    .map_err(|why| turn.failed(format!("{path}: {why}")))
            }
            other => Err(turn.failed(format!("cannot split {other}"))),
        }
    }

    /// The chunks of one source item, with the item rebuilt around each of them.
    fn chunks(&self, turn: &Turn<'_>, item: &Json) -> Result<Vec<Json>, Failure> {
        let (place, source) = match item {
            Json::Array(fields) => {
                let file = fields
                    .iter()
                    .position(|field| field.get("class").and_then(Json::as_str) == Some("File"));
                let place = self
                    .spec
                    .elem
                    .map(|elem| elem as usize)
                    .or(file)
                    .unwrap_or(0);
                let source = fields
                    .get(place)
                    .ok_or_else(|| turn.failed(format!("the item has no element {place}")))?;
                (Some(place), source.clone())
            }
            other => (None, other.clone()),
        };
        let mut text = self.text(turn, &source)?;
        // Text records end with a newline, also the last one.
        if matches!(self.spec.format, SplitFormat::Text(_))
            && !text.is_empty()
            && !text.ends_with('\n')
        {
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
        let splitter = self.splitter();
        let chunks = splitter
            .text(text)
            .map_err(|error| turn.failed(error.to_string()))?;
        let mut items = Vec::new();
        for chunk in chunks {
            let chunk = chunk.map_err(|error| turn.failed(error.to_string()))?;
            let mut value = groovy::to_json(&chunk).map_err(|why| turn.failed(why))?;
            if let (Some(header), Json::String(text)) = (&header, &value) {
                value = Json::String(format!("{header}{text}"));
            }
            items.push(match (place, item) {
                (Some(place), Json::Array(fields)) => {
                    let mut fields = fields.clone();
                    fields[place] = value;
                    Json::Array(fields)
                }
                _ => value,
            });
        }
        Ok(items)
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
mod tests {
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
