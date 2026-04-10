//! Builds the value tree of one YAML or JSON document from parser events.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Tag};

use crate::node::{Entry, Node, Position, Value};
use crate::scalar::resolve_plain;

/// Aliases may copy at most this many nodes in one document, which stops alias bombs.
const ALIAS_BUDGET: usize = 1_000_000;

/// Deeper nesting is refused, so recursion over trees cannot overflow the stack.
const DEPTH_LIMIT: usize = 1_000;

/// A syntax error with its position in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxError {
    pub message: String,
    pub position: Position,
}

/// Parses the first document of `text`. Later documents are ignored, as go-yaml does,
/// and text without a document gives `None`.
pub fn parse_yaml(text: &str) -> Result<Option<Node>, SyntaxError> {
    parse_at(text, Position { line: 1, column: 1 })
}

/// Parses like [`parse_yaml`] for text that starts at `origin` in its file.
pub(crate) fn parse_at(text: &str, origin: Position) -> Result<Option<Node>, SyntaxError> {
    let mut builder = Builder::new(origin, None);
    builder.run(text)?;
    Ok(builder.root)
}

/// Parses every document of `text`, as `argo lint` and `argo submit` read files that hold
/// several documents separated by `---`. Empty documents are left out.
pub fn parse_documents(text: &str) -> Result<Vec<Node>, SyntaxError> {
    let mut builder = Builder::new(Position { line: 1, column: 1 }, Some(Vec::new()));
    builder.run(text)?;
    let mut documents = builder.documents.unwrap_or_default();
    documents.extend(builder.root);
    documents.retain(|document| document.value != Value::Null);
    Ok(documents)
}

/// An open collection. A mapping holds its pending key until the value is complete.
enum Frame {
    Sequence {
        items: Vec<Node>,
        position: Position,
        anchor: usize,
    },
    Mapping {
        entries: Vec<Entry>,
        position: Position,
        anchor: usize,
        key: Option<Key>,
    },
}

enum Key {
    Merge(Position),
    Named(Node),
}

struct Builder {
    origin: Position,
    stack: Vec<Frame>,
    anchors: HashMap<usize, (Node, usize)>,
    budget: usize,
    root: Option<Node>,
    /// The finished documents, when all documents are read rather than the first.
    documents: Option<Vec<Node>>,
}

impl Builder {
    fn new(origin: Position, documents: Option<Vec<Node>>) -> Self {
        Builder {
            origin,
            stack: Vec::new(),
            anchors: HashMap::new(),
            budget: ALIAS_BUDGET,
            root: None,
            documents,
        }
    }

    fn run(&mut self, text: &str) -> Result<(), SyntaxError> {
        for event in Parser::new_from_str(text) {
            let (event, span) = event.map_err(|error| SyntaxError {
                message: error.info().to_owned(),
                position: self.position(*error.marker()),
            })?;
            let position = self.position(span.start);
            if self.accept(event, position)? {
                break;
            }
        }
        Ok(())
    }

    /// Handles one event and says whether the first document is complete.
    fn accept(&mut self, event: Event<'_>, position: Position) -> Result<bool, SyntaxError> {
        match event {
            Event::DocumentStart(_) | Event::DocumentEnd if self.documents.is_some() => {
                // Anchors belong to one document.
                self.anchors.clear();
                self.budget = ALIAS_BUDGET;
                if let (Some(documents), Some(root)) = (&mut self.documents, self.root.take()) {
                    documents.push(root);
                }
            }
            Event::DocumentStart(_) | Event::DocumentEnd => return Ok(self.root.is_some()),
            Event::Scalar(text, style, anchor, tag) => {
                let value = resolve(&text, style, tag.as_deref())
                    .map_err(|message| SyntaxError { message, position })?;
                let merge = style == ScalarStyle::Plain && text == "<<";
                let tagged_merge =
                    tag.is_some_and(|tag| tag.is_yaml_core_schema() && tag.suffix == "merge");
                let node = Node::new(value, position);
                self.remember(anchor, &node);
                self.complete(node, merge || tagged_merge)?;
            }
            Event::Alias(anchor) => {
                let Some((node, size)) = self.anchors.get(&anchor) else {
                    return Err(SyntaxError {
                        message: "unknown anchor".to_owned(),
                        position,
                    });
                };
                if *size > self.budget {
                    return Err(SyntaxError {
                        message: "aliases expand to too many values".to_owned(),
                        position,
                    });
                }
                self.budget -= size;
                let node = node.clone();
                self.complete(node, false)?;
            }
            Event::SequenceStart(..) | Event::MappingStart(..)
                if self.stack.len() >= DEPTH_LIMIT =>
            {
                return Err(SyntaxError {
                    message: format!("values are nested more than {DEPTH_LIMIT} levels deep"),
                    position,
                });
            }
            Event::SequenceStart(anchor, _) => self.stack.push(Frame::Sequence {
                items: Vec::new(),
                position,
                anchor,
            }),
            Event::MappingStart(anchor, _) => self.stack.push(Frame::Mapping {
                entries: Vec::new(),
                position,
                anchor,
                key: None,
            }),
            Event::SequenceEnd | Event::MappingEnd => {
                let node = match self.stack.pop() {
                    Some(Frame::Sequence {
                        items,
                        position,
                        anchor,
                    }) => self.close(Value::Sequence(items), position, anchor),
                    Some(Frame::Mapping {
                        entries,
                        position,
                        anchor,
                        ..
                    }) => self.close(Value::Mapping(entries), position, anchor),
                    None => return Ok(false),
                };
                self.complete(node, false)?;
            }
            Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
        }
        Ok(false)
    }

    fn close(&mut self, value: Value, position: Position, anchor: usize) -> Node {
        let node = Node::new(value, position);
        self.remember(anchor, &node);
        node
    }

    fn remember(&mut self, anchor: usize, node: &Node) {
        if anchor != 0 {
            self.anchors.insert(anchor, (node.clone(), size(node)));
        }
    }

    /// Adds a finished node to the open collection, or makes it the document.
    fn complete(&mut self, node: Node, merge: bool) -> Result<(), SyntaxError> {
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Frame::Sequence { items, .. }) => items.push(node),
            Some(Frame::Mapping { entries, key, .. }) => match key.take() {
                None if merge => *key = Some(Key::Merge(node.position)),
                None => *key = Some(Key::Named(node)),
                Some(Key::Merge(position)) => merge_into(entries, node, position)?,
                Some(Key::Named(name)) => {
                    let position = name.position;
                    insert(entries, key_text(name)?, position, node)?;
                }
            },
        }
        Ok(())
    }

    /// Converts a parser marker (line from 1, column from 0) to a position in the file.
    fn position(&self, marker: Marker) -> Position {
        let line = marker.line() as u32;
        let column = marker.col() as u32 + 1;
        if line <= 1 {
            Position {
                line: self.origin.line,
                column: self.origin.column + column - 1,
            }
        } else {
            Position {
                line: self.origin.line + line - 1,
                column,
            }
        }
    }
}

/// Mapping keys become strings the way sigs.k8s.io/yaml converts YAML to JSON.
fn key_text(key: Node) -> Result<String, SyntaxError> {
    let text = match key.value {
        Value::String(text) => text,
        Value::Integer(number) => number.to_string(),
        Value::Float(number) => (number as f32).to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => {
            return Err(SyntaxError {
                message: format!("{} cannot be a mapping key", key.describe()),
                position: key.position,
            });
        }
    };
    Ok(text)
}

/// Adds an entry. Argo reads YAML strictly, so a repeated key is an error.
fn insert(
    entries: &mut Vec<Entry>,
    key: String,
    position: Position,
    value: Node,
) -> Result<(), SyntaxError> {
    if let Some(first) = entries.iter().find(|entry| entry.key == key) {
        return Err(SyntaxError {
            message: format!(
                "duplicate key `{key}`, first set on line {}",
                first.position.line
            ),
            position,
        });
    }
    entries.push(Entry {
        key,
        position,
        value,
    });
    Ok(())
}

/// Applies a `<<` merge key: a mapping, or a sequence of mappings.
fn merge_into(
    entries: &mut Vec<Entry>,
    source: Node,
    position: Position,
) -> Result<(), SyntaxError> {
    let sources = match source.value {
        Value::Sequence(items) => items,
        _ => vec![source],
    };
    for source in sources {
        let Value::Mapping(merged) = source.value else {
            return Err(SyntaxError {
                message: "a merge key needs a mapping or a sequence of mappings".to_owned(),
                position: source.position,
            });
        };
        for entry in merged {
            insert(entries, entry.key, position, entry.value)?;
        }
    }
    Ok(())
}

/// Resolves a scalar from its text, style and tag, or explains why the tag does not fit.
fn resolve(text: &str, style: ScalarStyle, tag: Option<&Tag>) -> Result<Value, String> {
    let Some(tag) = tag.filter(|tag| tag.is_yaml_core_schema()) else {
        // Local tags like `!foo` are not resolvable, so go-yaml keeps the text.
        return Ok(match (style, tag) {
            (ScalarStyle::Plain, None) => resolve_plain(text),
            _ => Value::String(text.to_owned()),
        });
    };
    let value = resolve_plain(text);
    let fits = match tag.suffix.as_str() {
        "str" | "binary" | "timestamp" => return Ok(Value::String(text.to_owned())),
        "null" => matches!(value, Value::Null),
        "bool" => matches!(value, Value::Bool(_)),
        "int" => matches!(value, Value::Integer(_) | Value::Unsigned(_)),
        "float" => {
            return match value {
                Value::Integer(number) => Ok(Value::Float(number as f64)),
                Value::Unsigned(number) => Ok(Value::Float(number as f64)),
                Value::Float(_) => Ok(value),
                _ => Err(format!("cannot decode `{text}` as a !!float")),
            };
        }
        _ => return Ok(Value::String(text.to_owned())),
    };
    if fits {
        Ok(value)
    } else {
        Err(format!("cannot decode `{text}` as a !!{}", tag.suffix))
    }
}

fn size(node: &Node) -> usize {
    1 + match &node.value {
        Value::Sequence(items) => items.iter().map(size).sum(),
        Value::Mapping(entries) => entries.iter().map(|entry| size(&entry.value)).sum(),
        _ => 0,
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
