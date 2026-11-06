//! nf-fgbio 1.0.1: fgbio read structures and Illumina sample sheets, as fgbio's Scala objects.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Native, Object, PathValue, State as ObjectState, Value};

use crate::catalogue::{Kind, Plugin, State};
use crate::{Context, native, object};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["fromSampleSheet", "readStructure"],
    call: Some(call),
    methods: Some(methods),
    factories: &["fromSampleSheet"],
    ..Plugin::new("nf-fgbio", Kind::Functions, State::Implemented)
};

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("readStructure", [text]) => read_structure(&text.to_string()),
        ("fromSampleSheet", [Value::Path(path)]) => {
            let message = format!(
                "Missing process or function fromSampleSheet([{}])",
                path.text()
            );
            Err(thrown(Exception::new("MissingMethodException", message)))
        }
        ("fromSampleSheet", [path]) => sample_sheet(None, path, context),
        ("fromSampleSheet", [options @ Value::Map(_), path]) => {
            sample_sheet(Some(options), path, context)
        }
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
    Sample(Rc<Sample>),
    /// The extended attributes of a sample, as `Map` or `HashMap` prints them.
    Attributes(Rc<Attributes>),
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
            Fgbio::Sample(sample) => sample.text(),
            Fgbio::Attributes(attributes) => attributes.text(),
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
            Fgbio::Sample(_) => "com.fulcrumgenomics.illumina.Sample",
            Fgbio::Attributes(attributes) if attributes.hashed => {
                "scala.collection.immutable.HashMap"
            }
            Fgbio::Attributes(_) => "scala.collection.immutable.Map",
        }
    }

    fn value(self) -> Value {
        object(self.class(), self)
    }
}

fn option(value: Option<Value>) -> Value {
    Fgbio::Option(value).value()
}

fn text_option(text: &Option<String>) -> Value {
    option(text.as_deref().map(Value::string))
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
        (Fgbio::Sample(sample), name, arguments) => return sample.method(name, arguments),
        (Fgbio::Attributes(attributes), name, arguments) => {
            return attributes.method(name, arguments);
        }
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

/// A sample of a sample sheet, `com.fulcrumgenomics.illumina.Sample`.
#[derive(Debug)]
struct Sample {
    ordinal: i32,
    id: String,
    name: String,
    library: String,
    project: Option<String>,
    description: Option<String>,
    lane: Option<i32>,
    i7: Option<String>,
    i5: Option<String>,
    attributes: Rc<Attributes>,
}

/// The columns of a sample that fgbio does not know, by upper-case name.
#[derive(Debug)]
struct Attributes {
    /// Whether it prints as `HashMap`, which Scala gives maps of more than four entries,
    /// counting all columns of the sheet.
    hashed: bool,
    entries: Vec<(String, String)>,
}

impl Attributes {
    fn text(&self) -> String {
        if self.entries.is_empty() {
            return "Map()".into();
        }
        let mut entries: Vec<&(String, String)> = self.entries.iter().collect();
        // Scala's `HashMap` iterates in the order of its hash trie. uwe orders by the bits of
        // Java's string hash from the lowest up, which matches the recorded sheet.
        if self.hashed {
            entries.sort_by_key(|(key, _)| trie_order(java_hash(key)));
        }
        let texts: Vec<String> = entries.iter().map(|(k, v)| format!("{k} -> {v}")).collect();
        let name = if self.hashed { "HashMap" } else { "Map" };
        format!("{name}({})", texts.join(", "))
    }

    fn get(&self, key: &str) -> Option<&str> {
        let found = self.entries.iter().find(|(name, _)| *name == key);
        found.map(|(_, value)| value.as_str())
    }

    fn method(&self, name: &str, arguments: &[Value]) -> Result<Option<Value>, Error> {
        Ok(Some(match (name, arguments) {
            ("size", []) => Value::integer(self.entries.len() as i32),
            ("isEmpty", []) => Value::Boolean(self.entries.is_empty()),
            ("contains", [key]) => Value::Boolean(self.get(&key.to_string()).is_some()),
            ("get", [key]) => option(self.get(&key.to_string()).map(Value::string)),
            ("apply", [key]) => match self.get(&key.to_string()) {
                Some(value) => Value::string(value),
                None => {
                    let message = format!("key not found: {key}");
                    return Err(thrown(Exception::new("NoSuchElementException", message)));
                }
            },
            _ => return Ok(None),
        }))
    }
}

impl Sample {
    fn text(&self) -> String {
        let text = |value: &Option<String>| match value {
            Some(value) => format!("Some({value})"),
            None => "None".into(),
        };
        let lane = self.lane.map(|lane| lane.to_string());
        format!(
            "Sample({},{},{},{},{},{},{},{},{},{})",
            self.ordinal,
            self.id,
            self.name,
            self.library,
            text(&self.project),
            text(&self.description),
            text(&lane),
            text(&self.i7),
            text(&self.i5),
            self.attributes.text()
        )
    }

    fn method(&self, name: &str, arguments: &[Value]) -> Result<Option<Value>, Error> {
        let barcodes = || -> Vec<&str> {
            let indexes = [self.i7.as_deref(), self.i5.as_deref()];
            indexes
                .into_iter()
                .flatten()
                .filter(|bases| !bases.is_empty())
                .collect()
        };
        Ok(Some(match (name, arguments) {
            ("sampleOrdinal", []) => Value::integer(self.ordinal),
            ("sampleId", []) => Value::string(self.id.as_str()),
            ("sampleName", []) => Value::string(self.name.as_str()),
            ("libraryId", []) => Value::string(self.library.as_str()),
            ("project", []) => text_option(&self.project),
            ("description", []) => text_option(&self.description),
            ("lane", []) => option(self.lane.map(Value::integer)),
            ("i7IndexBases", []) => text_option(&self.i7),
            ("i5IndexBases", []) => text_option(&self.i5),
            ("extendedAttributes", []) => Fgbio::Attributes(self.attributes.clone()).value(),
            ("extendedAttribute", [key]) => {
                let key = key.to_string().to_uppercase();
                let value = self.attributes.get(&key).filter(|value| !value.is_empty());
                option(value.map(Value::string))
            }
            ("sampleBarcodes" | "sampleBarcodeBases", []) => {
                let items = vec![text_option(&self.i7), text_option(&self.i5)];
                Fgbio::Sequence("List", items).value()
            }
            ("sampleBarcodeString", []) => Value::string(barcodes().join("-")),
            ("sampleBarcodeBytes", []) => Value::Object(Rc::new(std::cell::RefCell::new(Object {
                class: "[B",
                state: ObjectState::Bytes(barcodes().concat().into_bytes().into()),
            }))),
            _ => return Ok(None),
        }))
    }
}

/// Java's `String.hashCode`.
fn java_hash(text: &str) -> u32 {
    let units = text.encode_utf16();
    units.fold(0u32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(u32::from(unit))
    })
}

/// The groups of five bits of a hash from the lowest, as a trie by hash visits them.
fn trie_order(hash: u32) -> [u32; 7] {
    std::array::from_fn(|level| (hash >> (5 * level)) & 31)
}

/// `channel.fromSampleSheet([lane: n,] path)`: the samples of the `[Data]` section of an
/// Illumina Experiment Manager sample sheet, in file order.
fn sample_sheet(
    options: Option<&Value>,
    path: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let mut lane = None;
    if let Some(Value::Map(options)) = options {
        for (key, value) in options.borrow().entries.iter() {
            let key = key.0.to_string();
            if key != "lane" {
                let message = format!(
                    "Unknown argument '{key}' for operator 'fromSampleSheet' -- Possible arguments: lane"
                );
                return Err(failure(&message));
            }
            match value {
                Value::Number(Number::Integer(number)) => lane = Some(*number).filter(|n| *n != 0),
                other => {
                    let message = format!(
                        "Value '{other}' cannot be used in in parameter 'lane' for operator \
                         'fromSampleSheet' -- Value don't match: class java.lang.Integer"
                    );
                    return Err(failure(&message));
                }
            }
        }
    }
    let given = path.to_string();
    let file = PathValue::parse(&given, &context.host.launch_dir());
    if context.host.metadata(&file)?.is_none() {
        let message = format!("{given} (No such file or directory)");
        return Err(thrown(Exception::new("FileNotFoundException", message)));
    }
    let bytes = context.host.read(&file)?;
    let samples = samples(&String::from_utf8_lossy(&bytes), lane).map_err(thrown)?;
    Ok(Value::list(
        samples
            .into_iter()
            .map(|s| Fgbio::Sample(Rc::new(s)).value())
            .collect(),
    ))
}

/// The fields of a line, split at commas outside double quotes, which stay in the fields.
fn fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(String::new());
                continue;
            }
            _ => {}
        }
        if let Some(field) = fields.last_mut() {
            field.push(c);
        }
    }
    fields
}

const STANDARD: [&str; 8] = [
    "SAMPLE_ID",
    "SAMPLE_NAME",
    "LIBRARY_ID",
    "SAMPLE_PROJECT",
    "DESCRIPTION",
    "LANE",
    "INDEX",
    "INDEX2",
];

/// The samples of a sheet, of one lane when `lane` is given.
fn samples(text: &str, lane: Option<i32>) -> Result<Vec<Sample>, Exception> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|line| line.starts_with("[Data]"));
    let Some(header) = start.and_then(|start| lines.get(start + 1)) else {
        let message = "Could not find the header for sample data.";
        return Err(Exception::illegal_argument(message));
    };
    let first = start.unwrap_or_default() + 2;
    let header: Vec<String> = fields(header)
        .iter()
        .map(|f| f.trim().to_uppercase())
        .collect();
    let empty = |line: &str| fields(line).iter().all(|field| field.trim().is_empty());
    let mut rows = &lines[first.min(lines.len())..];
    while rows.last().is_some_and(|line| empty(line)) {
        rows = &rows[..rows.len() - 1];
    }
    let mut samples = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        if empty(line) {
            let message = format!(
                "Found empty line on row {} of the sample data section.",
                row + 1
            );
            return Err(Exception::illegal_argument(message));
        }
        let raw = fields(line);
        if raw.len() != header.len() {
            let message = format!(
                "Found a line with a mismatching number of columns on line #{}: {}",
                first + row + 1,
                raw.join(",")
            );
            return Err(Exception::illegal_argument(message));
        }
        let cell = |name: &str| {
            let found = header.iter().position(|column| column == name);
            found.map(|index| raw[index].trim().to_owned())
        };
        let present = |name: &str| cell(name).filter(|value| !value.is_empty());
        let sample_lane = match present("LANE") {
            Some(text) => match text.parse::<i32>() {
                Ok(number) => Some(number),
                Err(_) => return Err(Exception::number_format(&text)),
            },
            None => None,
        };
        if lane.is_some() && sample_lane != lane {
            continue;
        }
        let ordinal = samples.len() as i32 + 1;
        let required = |name: &str, column: &str| {
            present(column).ok_or_else(|| {
                let message = format!("Sample at ordinal #{ordinal} is missing: {name}");
                Exception::illegal_argument(message)
            })
        };
        let name = required("Sample_Name", "SAMPLE_NAME")?;
        let id = required("Sample_ID", "SAMPLE_ID")?;
        let attributes = (header.iter().zip(&raw))
            .filter(|(column, _)| !STANDARD.contains(&column.as_str()))
            .map(|(column, value)| (column.clone(), value.trim().to_owned()))
            .collect();
        samples.push(Sample {
            ordinal,
            library: present("LIBRARY_ID").unwrap_or_else(|| id.clone()),
            id,
            name,
            project: present("SAMPLE_PROJECT"),
            description: present("DESCRIPTION"),
            lane: sample_lane,
            i7: present("INDEX"),
            i5: present("INDEX2"),
            attributes: Rc::new(Attributes {
                hashed: header.len() > 4,
                entries: attributes,
            }),
        });
    }
    unique(&samples)?;
    Ok(samples)
}

/// Checks that the samples of each lane, with those without a lane, and the samples without
/// a lane have unique ids and unique names with library ids.
fn unique(samples: &[Sample]) -> Result<(), Exception> {
    let mut lanes: Vec<Option<i32>> = Vec::new();
    for sample in samples {
        if sample.lane.is_some() && !lanes.contains(&sample.lane) {
            lanes.push(sample.lane);
        }
    }
    lanes.push(None);
    for lane in lanes {
        let group: Vec<&Sample> = (samples.iter())
            .filter(|sample| sample.lane.is_none() || sample.lane == lane)
            .collect();
        let ids = repeated(group.iter().map(|sample| sample.id.clone()));
        if !ids.is_empty() {
            let message = format!(
                "requirement failed: Sample identifiers were not unique: {}",
                ids.join(", ")
            );
            return Err(Exception::illegal_argument(message));
        }
        let keys = repeated(group.iter().map(|sample| match sample.lane {
            Some(lane) => format!("{} {} {lane}", sample.name, sample.library),
            None => format!("{} {}", sample.name, sample.library),
        }));
        if !keys.is_empty() {
            let message = format!(
                "requirement failed: Sample name and library identifier combinations were not \
                 unique: {}",
                keys.join(", ")
            );
            return Err(Exception::illegal_argument(message));
        }
    }
    Ok(())
}

/// The values that occur more than once, each once, in the order they first repeat.
fn repeated(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    let mut repeated = Vec::new();
    for value in values {
        if seen.contains(&value) {
            if !repeated.contains(&value) {
                repeated.push(value);
            }
        } else {
            seen.push(value);
        }
    }
    repeated
}
