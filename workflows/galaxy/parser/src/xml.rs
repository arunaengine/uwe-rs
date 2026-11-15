//! Reads XML text into elements with the text and tail model of lxml, which Galaxy uses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use quick_xml::events::{BytesStart, Event};

use crate::Error;

/// An element with its text before the first child and its tail after the end tag. Comments
/// are dropped and their surrounding text joined, as lxml does without comments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Element>,
    pub tail: String,
    /// The 1-based line of the start tag in its file.
    pub line: usize,
}

/// Reads the root element of `text`; `file` names it in errors.
pub fn read(text: &str, file: &str) -> Result<Element, Error> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    loop {
        let at = reader.buffer_position() as usize;
        let line = 1 + text.as_bytes()[..at.min(text.len())]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        let failed = |source| Error::Xml {
            file: file.to_owned(),
            line,
            source,
        };
        let event = reader.read_event().map_err(failed)?;
        match event {
            Event::Start(start) => open.push(started(&start, line, file)?),
            Event::Empty(start) => {
                let element = started(&start, line, file)?;
                close(&mut open, &mut root, element);
            }
            Event::End(_) => {
                let Some(element) = open.pop() else {
                    return Err(invalid(file, line, "an end tag without a start tag"));
                };
                close(&mut open, &mut root, element);
            }
            Event::Text(content) => append(&mut open, &content.xml10_content()),
            Event::CData(content) => append(&mut open, &content.xml10_content()),
            Event::GeneralRef(reference) => {
                let name = reference.xml10_content();
                let character = match reference.resolve_char_ref() {
                    Ok(Some(character)) => character,
                    Ok(None) => match &*name {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        other => {
                            return Err(invalid(file, line, &format!("unknown entity &{other};")));
                        }
                    },
                    Err(source) => return Err(failed(source)),
                };
                append(&mut open, &character.to_string());
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !open.is_empty() {
        return Err(invalid(
            file,
            text.lines().count(),
            "the document ends inside an element",
        ));
    }
    root.ok_or_else(|| invalid(file, 1, "the document has no element"))
}

impl Element {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The first child of this name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// All text inside the element in document order, like lxml's `itertext`.
    pub fn content(&self) -> String {
        let mut text = self.text.clone();
        for child in &self.children {
            text.push_str(&child.content());
            text.push_str(&child.tail);
        }
        text
    }
}

fn started(start: &BytesStart, line: usize, file: &str) -> Result<Element, Error> {
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| invalid(file, line, &error.to_string()))?;
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|source| Error::Xml {
                file: file.to_owned(),
                line,
                source,
            })?;
        let key = attribute.key.as_ref().to_owned();
        attributes.push((key, value.into_owned()));
    }
    Ok(Element {
        name: start.name().as_ref().to_owned(),
        attributes,
        line,
        ..Element::default()
    })
}

fn close(open: &mut [Element], root: &mut Option<Element>, element: Element) {
    match open.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

/// Adds text to the open element, or to the tail of its last child.
fn append(open: &mut [Element], text: &str) {
    let Some(parent) = open.last_mut() else {
        return;
    };
    match parent.children.last_mut() {
        Some(child) => child.tail.push_str(text),
        None => parent.text.push_str(text),
    }
}

fn invalid(file: &str, line: usize, message: &str) -> Error {
    Error::Document {
        file: file.to_owned(),
        line,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_text_tails() {
        let root = read(
            "<?xml version=\"1.0\"?>\n<tool id=\"a&amp;b\">\n  <command><![CDATA[x < $y]]> &#38; z</command><!-- note -->tail</tool>",
            "tool.xml",
        )
        .expect("valid XML");
        assert_eq!(root.attribute("id"), Some("a&b"));
        assert_eq!(root.text, "\n  ");
        let command = root.child("command").expect("a command");
        assert_eq!(command.text, "x < $y & z");
        assert_eq!(command.tail, "tail", "the comment is dropped");
        assert_eq!(command.line, 3);
        assert_eq!(root.content(), "\n  x < $y & ztail");
    }

    #[test]
    fn refuses_broken_xml() {
        let error = read("<tool>\n<inputs></tool>", "broken.xml").expect_err("mismatched tags");
        assert!(error.to_string().starts_with("broken.xml:"), "{error}");
        assert!(read("text only", "empty.xml").is_err());
    }
}
