//! nf-fgbio 1.0.1: fgbio read structures, as fgbio's Scala objects.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{Native, Value};

use crate::catalogue::{Kind, Plugin, State};
use crate::{Context, native, object};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["readStructure"],
    call: Some(call),
    methods: Some(methods),
    ..Plugin::new("nf-fgbio", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("readStructure", [text]) => read_structure(&text.to_string()),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-fgbio` with these arguments"),
            span: Default::default(),
        }),
    }
}

/// A segment of a read structure: where it starts, its length or `None` for the rest of the
/// read, and its kind `T`, `B`, `M` or `S`.
#[derive(Clone, Debug, PartialEq)]
struct Segment {
    offset: usize,
    length: Option<usize>,
    kind: char,
}

impl Segment {
    fn text(&self) -> String {
        match self.length {
            Some(length) => format!("{length}{}", self.kind),
            None => format!("+{}", self.kind),
        }
    }
}

/// The objects of fgbio and Scala that the plugin's functions and methods give.
#[derive(Debug)]
enum Fgbio {
    /// `ReadStructure`
    Structure(Vec<Segment>),
    /// `ReadSegment`
    Segment(Segment),
    /// `SegmentType`, by its letter.
    Kind(char),
    /// Scala's `Some(value)` or `None`.
    Option(Option<Value>),
    /// A Scala `Vector` or `List`, by the name it prints with.
    Sequence(&'static str, Vec<Value>),
    /// `SubReadWithoutQuals` or `SubReadWithQuals`, with the segment as long as what it took.
    SubRead {
        bases: String,
        qualities: Option<String>,
        segment: Segment,
    },
}

impl Native for Fgbio {
    fn plugin(&self) -> &'static str {
        "nf-fgbio"
    }

    fn text(&self) -> String {
        match self {
            Fgbio::Structure(segments) => segments.iter().map(Segment::text).collect(),
            Fgbio::Segment(segment) => segment.text(),
            Fgbio::Kind(kind) => kind.to_string(),
            Fgbio::Option(Some(value)) => format!("Some({value})"),
            Fgbio::Option(None) => "None".into(),
            Fgbio::Sequence(name, items) => format!("{name}({})", joined(items)),
            Fgbio::SubRead {
                bases,
                qualities: None,
                segment,
            } => format!("SubReadWithoutQuals({bases},{})", segment.text()),
            Fgbio::SubRead {
                bases,
                qualities: Some(qualities),
                segment,
            } => format!("SubReadWithQuals({bases},{qualities},{})", segment.text()),
        }
    }

    fn key(&self) -> Option<String> {
        match self {
            Fgbio::Structure(_) | Fgbio::Kind(_) => Some(self.text()),
            Fgbio::Segment(segment) => Some(format!("{}{}", segment.offset, segment.text())),
            _ => None,
        }
    }

    fn calls_closure(&self, method: &str) -> bool {
        matches!(self, Fgbio::Option(None)) && method == "getOrElse"
    }
}

impl Fgbio {
    fn class(&self) -> &'static str {
        match self {
            Fgbio::Structure(_) => "com.fulcrumgenomics.util.ReadStructure",
            Fgbio::Segment(_) => "com.fulcrumgenomics.util.ReadSegment",
            Fgbio::Kind('T') => "com.fulcrumgenomics.util.SegmentType$Template$",
            Fgbio::Kind('B') => "com.fulcrumgenomics.util.SegmentType$SampleBarcode$",
            Fgbio::Kind('M') => "com.fulcrumgenomics.util.SegmentType$MolecularBarcode$",
            Fgbio::Kind(_) => "com.fulcrumgenomics.util.SegmentType$Skip$",
            Fgbio::Option(Some(_)) => "scala.Some",
            Fgbio::Option(None) => "scala.None$",
            Fgbio::Sequence("List", _) => "scala.collection.immutable.$colon$colon",
            Fgbio::Sequence(..) => "scala.collection.immutable.Vector1",
            Fgbio::SubRead {
                qualities: None, ..
            } => "com.fulcrumgenomics.util.SubReadWithoutQuals",
            Fgbio::SubRead { .. } => "com.fulcrumgenomics.util.SubReadWithQuals",
        }
    }

    fn value(self) -> Value {
        object(self.class(), self)
    }
}

fn option(value: Option<Value>) -> Value {
    Fgbio::Option(value).value()
}

fn joined(items: &[Value]) -> String {
    let texts: Vec<String> = items.iter().map(Value::to_string).collect();
    texts.join(", ")
}

/// Runs a method of an fgbio object, `None` for a method it does not have.
fn methods(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    _: &mut Context,
) -> Result<Option<Value>, Error> {
    let Some(state) = native::<Fgbio>(object) else {
        return Ok(None);
    };
    let integer = |value: usize| Value::integer(value as i32);
    Ok(Some(match (&*state, name, arguments.as_slice()) {
        (_, "toString", []) => Value::string(state.text()),
        (Fgbio::Structure(segments), name, arguments) => {
            return structure_method(segments, name, arguments);
        }
        (Fgbio::Segment(segment), "offset", []) => integer(segment.offset),
        (Fgbio::Segment(segment), "length", []) => option(segment.length.map(integer)),
        (Fgbio::Segment(segment), "kind", []) => Fgbio::Kind(segment.kind).value(),
        (Fgbio::Segment(segment), "hasFixedLength", []) => Value::Boolean(segment.length.is_some()),
        (Fgbio::Segment(segment), "fixedLength", []) => match segment.length {
            Some(length) => integer(length),
            None => {
                let message = format!(
                    "fixedLength called on variable length segment: {}",
                    segment.text()
                );
                return Err(thrown(Exception::new("IllegalStateException", message)));
            }
        },
        (Fgbio::Kind(kind), "code", []) => Value::Character(*kind as u16),
        (Fgbio::Option(value), "get", []) => match value {
            Some(value) => value.clone(),
            None => {
                let error = Exception::new("NoSuchElementException", "None.get");
                return Err(thrown(error));
            }
        },
        (Fgbio::Option(value), "isDefined" | "nonEmpty", []) => Value::Boolean(value.is_some()),
        (Fgbio::Option(value), "isEmpty", []) => Value::Boolean(value.is_none()),
        (Fgbio::Option(value), "getOrElse", [default]) => {
            value.clone().unwrap_or_else(|| default.clone())
        }
        (Fgbio::Sequence(_, items), name, arguments) => {
            return sequence_method(items, name, arguments);
        }
        (Fgbio::SubRead { bases, .. }, "bases", []) => Value::string(bases.as_str()),
        (Fgbio::SubRead { qualities, .. }, "quals", []) if qualities.is_some() => {
            Value::string(qualities.as_deref().unwrap_or_default())
        }
        (Fgbio::SubRead { segment, .. }, "segment", []) => Fgbio::Segment(segment.clone()).value(),
        (Fgbio::SubRead { segment, .. }, "kind", []) => Fgbio::Kind(segment.kind).value(),
        _ => return Ok(None),
    }))
}

fn structure_method(
    segments: &[Segment],
    name: &str,
    arguments: &[Value],
) -> Result<Option<Value>, Error> {
    let canonical: String = segments.iter().map(Segment::text).collect();
    let values = |kind: Option<char>| -> Vec<Value> {
        let chosen = segments
            .iter()
            .filter(|segment| kind.is_none_or(|k| segment.kind == k));
        chosen
            .map(|segment| Fgbio::Segment(segment.clone()).value())
            .collect()
    };
    let vector = |items: Vec<Value>| Fgbio::Sequence("Vector", items).value();
    Ok(Some(match (name, arguments) {
        ("segments", []) => vector(values(None)),
        ("length" | "size", []) => Value::integer(segments.len() as i32),
        ("apply", [index]) => {
            return sequence_method(&values(None), "apply", std::slice::from_ref(index));
        }
        ("head", []) => return sequence_method(&values(None), "head", &[]),
        ("hasFixedLength", []) => {
            Value::Boolean(segments.last().is_some_and(|s| s.length.is_some()))
        }
        ("fixedLength", []) => match segments
            .iter()
            .map(|segment| segment.length)
            .sum::<Option<usize>>()
        {
            Some(total) => Value::integer(total as i32),
            None => {
                return Err(failure(&format!(
                    "requirement failed: fixedLength called on variable length segment: {canonical}"
                )));
            }
        },
        ("templateSegments", []) => vector(values(Some('T'))),
        ("sampleBarcodeSegments", []) => vector(values(Some('B'))),
        ("molecularBarcodeSegments", []) => vector(values(Some('M'))),
        ("skipSegments", []) => vector(values(Some('S'))),
        ("withVariableLastSegment", []) => {
            let mut changed = segments.to_vec();
            if let Some(last) = changed.last_mut() {
                last.length = None;
            }
            Fgbio::Structure(changed).value()
        }
        ("extract", [bases]) => vector(extract(segments, &bases.to_string(), None)?),
        ("extract", [bases, qualities]) => {
            let qualities = qualities.to_string();
            vector(extract(segments, &bases.to_string(), Some(&qualities))?)
        }
        _ => return Ok(None),
    }))
}

/// The part of the read of each segment: a fixed segment takes its length, the last `+`
/// segment the rest.
fn extract(
    segments: &[Segment],
    bases: &str,
    qualities: Option<&str>,
) -> Result<Vec<Value>, Error> {
    let bases: Vec<char> = bases.chars().collect();
    let qualities: Option<Vec<char>> = qualities.map(|text| text.chars().collect());
    if qualities.as_ref().is_some_and(|q| q.len() != bases.len()) {
        return Err(thrown(Exception::assertion("assertion failed")));
    }
    let mut parts = Vec::new();
    for segment in segments {
        let end = segment
            .length
            .map_or(bases.len(), |length| segment.offset + length);
        if end > bases.len() || segment.offset > bases.len() {
            return Err(failure(&format!(
                "requirement failed: Read ends before end of segment: {}",
                segment.text()
            )));
        }
        let taken = Segment {
            length: Some(end - segment.offset),
            ..segment.clone()
        };
        let part = |text: &[char]| text[segment.offset..end].iter().collect::<String>();
        parts.push(
            Fgbio::SubRead {
                bases: part(&bases),
                qualities: qualities.as_deref().map(part),
                segment: taken,
            }
            .value(),
        );
    }
    Ok(parts)
}

fn sequence_method(
    items: &[Value],
    name: &str,
    arguments: &[Value],
) -> Result<Option<Value>, Error> {
    Ok(Some(match (name, arguments) {
        ("size" | "length", []) => Value::integer(items.len() as i32),
        ("isEmpty", []) => Value::Boolean(items.is_empty()),
        ("nonEmpty", []) => Value::Boolean(!items.is_empty()),
        ("head", []) => match items.first() {
            Some(item) => item.clone(),
            None => {
                let error = Exception::new("NoSuchElementException", "head of empty list");
                return Err(thrown(error));
            }
        },
        ("last", []) => match items.last() {
            Some(item) => item.clone(),
            None => {
                let error = Exception::new("NoSuchElementException", "last of empty list");
                return Err(thrown(error));
            }
        },
        ("apply", [Value::Number(index)]) => {
            let found = usize::try_from(index.to_i64())
                .ok()
                .and_then(|i| items.get(i));
            match found {
                Some(item) => item.clone(),
                None => {
                    let message = format!(
                        "{index} is out of bounds (min 0, max {})",
                        items.len() as i64 - 1
                    );
                    return Err(thrown(Exception::new("IndexOutOfBoundsException", message)));
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// A `ReadStructure`, which prints as its segments: a length, or `+` for the rest of the read,
/// then the kind `T`, `B`, `M` or `S`. Case and blanks do not matter.
fn read_structure(text: &str) -> Result<Value, Error> {
    let tidied: String = text
        .to_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let characters: Vec<char> = tidied.chars().collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut offset = 0;
    let mut index = 0;
    while index < characters.len() {
        let start = index;
        let length = match characters[index] {
            '+' => {
                index += 1;
                None
            }
            c if c.is_ascii_digit() => {
                while index < characters.len() && characters[index].is_ascii_digit() {
                    index += 1;
                }
                let digits: String = characters[start..index].iter().collect();
                match digits.parse::<usize>() {
                    Ok(length) => Some(length),
                    Err(_) => return Err(failure(&format!("For input string: \"{digits}\""))),
                }
            }
            _ => {
                return Err(invalid(
                    "Read structure missing length information",
                    &characters,
                    start,
                    start + 1,
                ));
            }
        };
        if index == characters.len() {
            return Err(invalid(
                "Read structure with invalid segment",
                &characters,
                start,
                index,
            ));
        }
        let kind = characters[index];
        index += 1;
        if !matches!(kind, 'T' | 'B' | 'M' | 'S') {
            return Err(invalid(
                "Read structure segment had unknown type",
                &characters,
                start,
                index,
            ));
        }
        segments.push(Segment {
            offset,
            length,
            kind,
        });
        offset += length.unwrap_or(0);
    }
    if segments.is_empty() {
        return Err(failure(
            "requirement failed: Can't create a read structure with zero segments.",
        ));
    }
    if segments[..segments.len() - 1]
        .iter()
        .any(|s| s.length.is_none())
    {
        return Err(failure(&format!(
            "requirement failed: Variable length (+) can only be used in the last segment: {tidied}"
        )));
    }
    if segments.iter().any(|segment| segment.length == Some(0)) {
        return Err(failure(&format!(
            "Read structure contained zero length segments: {tidied}"
        )));
    }
    Ok(Fgbio::Structure(segments).value())
}

/// A parse error that marks the part in brackets, like `10B[20C]+T`.
fn invalid(message: &str, characters: &[char], start: usize, end: usize) -> Error {
    let part = |range: std::ops::Range<usize>| characters[range].iter().collect::<String>();
    failure(&format!(
        "{message}: {}[{}]{}",
        part(0..start),
        part(start..end),
        part(end..characters.len())
    ))
}

/// The `IllegalArgumentException` of fgbio, which the call by reflection wraps.
fn failure(message: &str) -> Error {
    thrown(Exception::illegal_argument(message))
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
