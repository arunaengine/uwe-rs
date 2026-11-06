//! nf-pregex 1.1.0: builds readable regular expressions from patterns.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::{Native, Value};

use crate::catalogue::{Kind, Plugin, State};
use crate::{Context, native, object};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: FUNCTIONS,
    call: Some(call),
    ..Plugin::new("nf-pregex", Kind::Functions, State::Implemented)
};

const FUNCTIONS: &[&str] = &[
    "AlignmentExtension",
    "AnyChar",
    "AnyOf",
    "AtLeast",
    "BedExtension",
    "Capture",
    "CharClass",
    "CharRange",
    "Chromosome",
    "Digit",
    "DNASequence",
    "DNASequenceWithAmbiguity",
    "Either",
    "EndOfLine",
    "EndOfString",
    "Exactly",
    "FastaExtension",
    "FastqExtension",
    "GffGtfExtension",
    "Group",
    "Literal",
    "MultiRange",
    "NamedCapture",
    "NamedGroup",
    "NotCharClass",
    "OneOrMore",
    "Optional",
    "PhredQuality",
    "ProteinSequence",
    "ProteinSequenceWithAmbiguity",
    "Range",
    "ReadPair",
    "Sequence",
    "StartOfLine",
    "StartOfString",
    "StrictChromosome",
    "StrictDNASequence",
    "StrictProteinSequence",
    "VcfExtension",
    "WordChar",
    "Whitespace",
    "ZeroOrMore",
];

/// A node of a pattern, which gives its part of the regex.
#[derive(Debug)]
enum Node {
    Literal(String),
    Sequence(Vec<Rc<Node>>),
    Either(Vec<String>),
    AnyOf(Vec<Rc<Node>>),
    Raw(String),
    Optional(Rc<Node>),
    OneOrMore(Rc<Node>),
    ZeroOrMore(Rc<Node>),
    Exactly(Rc<Node>, i64),
    Range(Rc<Node>, i64, i64),
    AtLeast(Rc<Node>, i64),
    AnyChar,
    Digit,
    WordChar,
    Whitespace,
    StartOfLine,
    EndOfLine,
    StartOfString,
    EndOfString,
    CharRange(char, char),
    MultiRange(Vec<(char, char)>),
    CharClass { chars: String, negated: bool },
    Group(Rc<Node>),
    NamedGroup(String, Rc<Node>),
}

/// The objects of the plugin: patterns.
#[derive(Debug)]
enum Pregex {
    Pattern(Rc<Node>),
}

impl Native for Pregex {
    fn plugin(&self) -> &'static str {
        "nf-pregex"
    }

    fn text(&self) -> String {
        match self {
            Pregex::Pattern(node) => node.regex(),
        }
    }
}

impl Node {
    fn regex(&self) -> String {
        match self {
            Node::Literal(text) => escaped(text),
            Node::Sequence(children) => children.iter().map(|child| child.regex()).collect(),
            Node::Either(items) if items.len() == 1 => escaped(&items[0]),
            Node::Either(items) => {
                let items: Vec<String> = items.iter().map(|item| escaped(item)).collect();
                format!("(?:{})", items.join("|"))
            }
            Node::AnyOf(children) if children.len() == 1 => children[0].regex(),
            Node::AnyOf(children) => {
                let items: Vec<String> = children.iter().map(|child| child.regex()).collect();
                format!("(?:{})", items.join("|"))
            }
            Node::Raw(regex) => regex.clone(),
            Node::Optional(child) => format!("(?:{})?", child.regex()),
            Node::OneOrMore(child) => format!("(?:{})+", child.regex()),
            Node::ZeroOrMore(child) => format!("(?:{})*", child.regex()),
            Node::Exactly(child, count) => format!("{}{{{count}}}", bare(&child.regex())),
            Node::Range(child, min, max) => format!("{}{{{min},{max}}}", bare(&child.regex())),
            Node::AtLeast(child, min) => format!("{}{{{min},}}", bare(&child.regex())),
            Node::AnyChar => ".".into(),
            Node::Digit => "\\d".into(),
            Node::WordChar => "\\w".into(),
            Node::Whitespace => "\\s".into(),
            Node::StartOfLine => "^".into(),
            Node::EndOfLine => "$".into(),
            Node::StartOfString => "\\A".into(),
            Node::EndOfString => "\\z".into(),
            Node::CharRange(start, end) => format!("[{}]", range(*start, *end)),
            Node::MultiRange(ranges) => {
                let inner: String = ranges.iter().map(|(s, e)| range(*s, *e)).collect();
                format!("[{inner}]")
            }
            Node::CharClass { chars, negated } => {
                let chars = chars
                    .replace('\\', "\\\\")
                    .replace('^', "\\^")
                    .replace(']', "\\]")
                    .replace('-', "\\-");
                format!("[{}{chars}]", if *negated { "^" } else { "" })
            }
            Node::Group(child) => format!("({})", child.regex()),
            Node::NamedGroup(name, child) => format!("(?<{name}>{})", child.regex()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Node::Literal(_) => "Literal",
            Node::Sequence(_) => "Sequence",
            Node::Either(_) => "Either",
            Node::AnyOf(_) => "AnyOf",
            Node::Raw(_) => "Raw",
            Node::Optional(_) => "Optional",
            Node::OneOrMore(_) => "OneOrMore",
            Node::ZeroOrMore(_) => "ZeroOrMore",
            Node::Exactly(..) => "Exactly",
            Node::Range(..) => "Range",
            Node::AtLeast(..) => "AtLeast",
            Node::AnyChar => "AnyChar",
            Node::Digit => "Digit",
            Node::WordChar => "WordChar",
            Node::Whitespace => "Whitespace",
            Node::StartOfLine => "StartOfLine",
            Node::EndOfLine => "EndOfLine",
            Node::StartOfString => "StartOfString",
            Node::EndOfString => "EndOfString",
            Node::CharRange(..) => "CharRange",
            Node::MultiRange(_) => "MultiRange",
            Node::CharClass { .. } => "CharClass",
            Node::Group(_) => "Group",
            Node::NamedGroup(..) => "NamedGroup",
        }
    }
}

/// The text with a backslash before each character that regexes give a meaning.
fn escaped(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\.*+?^$(){}[]|-".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A regex that a quantifier may follow as it is: one escaped character, `.` or a class.
fn bare(regex: &str) -> String {
    let characters: Vec<char> = regex.chars().collect();
    let single = characters.len() == 2 && characters[0] == '\\' || regex == ".";
    let class = characters.len() > 2
        && regex.starts_with('[')
        && regex.ends_with(']')
        && !regex.contains(['\n', '\r']);
    match single || class {
        true => regex.to_owned(),
        false => format!("(?:{regex})"),
    }
}

/// `s-e` of a class, with the characters that a class gives a meaning escaped.
fn range(start: char, end: char) -> String {
    let one = |c: char| match c {
        '\\' | '^' | ']' | '-' => format!("\\{c}"),
        other => other.to_string(),
    };
    format!("{}-{}", one(start), one(end))
}

fn pattern(node: Node) -> Value {
    shared(Rc::new(node))
}

/// A pattern object of a node, with the class of its kind.
fn shared(node: Rc<Node>) -> Value {
    let class = match node.kind() {
        "Literal" => "io.nextflow.pregex.PRegEx$Literal",
        "Sequence" => "io.nextflow.pregex.PRegEx$Sequence",
        "Either" => "io.nextflow.pregex.PRegEx$Either",
        "AnyOf" => "io.nextflow.pregex.PRegEx$AnyOf",
        "Raw" => "io.nextflow.pregex.PRegEx$Raw",
        "Optional" => "io.nextflow.pregex.PRegEx$Optional",
        "OneOrMore" => "io.nextflow.pregex.PRegEx$OneOrMore",
        "ZeroOrMore" => "io.nextflow.pregex.PRegEx$ZeroOrMore",
        "Exactly" => "io.nextflow.pregex.PRegEx$Exactly",
        "Range" => "io.nextflow.pregex.PRegEx$Range",
        "AtLeast" => "io.nextflow.pregex.PRegEx$AtLeast",
        "AnyChar" => "io.nextflow.pregex.PRegEx$AnyChar",
        "Digit" => "io.nextflow.pregex.PRegEx$Digit",
        "WordChar" => "io.nextflow.pregex.PRegEx$WordChar",
        "Whitespace" => "io.nextflow.pregex.PRegEx$Whitespace",
        "StartOfLine" => "io.nextflow.pregex.PRegEx$StartOfLine",
        "EndOfLine" => "io.nextflow.pregex.PRegEx$EndOfLine",
        "StartOfString" => "io.nextflow.pregex.PRegEx$StartOfString",
        "EndOfString" => "io.nextflow.pregex.PRegEx$EndOfString",
        "CharRange" => "io.nextflow.pregex.PRegEx$CharRange",
        "MultiRange" => "io.nextflow.pregex.PRegEx$MultiRange",
        "CharClass" => "io.nextflow.pregex.PRegEx$CharClass",
        "Group" => "io.nextflow.pregex.PRegEx$Group",
        _ => "io.nextflow.pregex.PRegEx$NamedGroup",
    };
    object(class, Pregex::Pattern(node))
}

/// The node of a pattern object.
fn node_of(value: &Value) -> Option<Rc<Node>> {
    match native::<Pregex>(value).as_deref() {
        Some(Pregex::Pattern(node)) => Some(node.clone()),
        _ => None,
    }
}

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let node = |index: usize| arguments.get(index).and_then(node_of);
    let number = |index: usize| match arguments.get(index) {
        Some(Value::Number(number)) => Some(number.to_i64()),
        _ => None,
    };
    let text = |index: usize| arguments.get(index).map(Value::to_string);
    let raw = |regex: &str| Ok(pattern(Node::Raw(regex.into())));
    let class = |chars: &str| {
        Rc::new(Node::CharClass {
            chars: chars.into(),
            negated: false,
        })
    };
    let sequence = |chars: &str| Ok(pattern(Node::OneOrMore(class(chars))));
    match (name, arguments.len()) {
        ("Literal", 1) => Ok(pattern(Node::Literal(text(0).unwrap_or_default()))),
        ("Sequence", 1) => {
            let children = nodes(&arguments[0], "Sequence requires at least one pattern")?;
            Ok(pattern(Node::Sequence(children)))
        }
        ("Either", 1) => either(&arguments[0]),
        ("AnyOf", 1) => {
            let children = nodes(&arguments[0], "AnyOf requires at least one pattern")?;
            Ok(pattern(Node::AnyOf(children)))
        }
        ("Optional" | "OneOrMore" | "ZeroOrMore" | "Group" | "Capture", 1) => {
            let child = node(0).ok_or_else(|| arity(name))?;
            Ok(quantified(name, child))
        }
        ("Exactly", 2) | ("AtLeast", 2) => {
            let (child, count) = (node(0).ok_or_else(|| arity(name))?, number(1));
            counted(name, child, count.ok_or_else(|| arity(name))?, None).map_err(failure)
        }
        ("Range", 3) => {
            let child = node(0).ok_or_else(|| arity(name))?;
            let (min, max) = (number(1), number(2));
            let (min, max) = min.zip(max).ok_or_else(|| arity(name))?;
            counted(name, child, min, Some(max)).map_err(failure)
        }
        ("Group" | "NamedCapture", 2) => {
            let child = node(1).ok_or_else(|| arity(name))?;
            named(&text(0).unwrap_or_default(), child).map_err(failure)
        }
        ("NamedGroup", 2) => {
            let child = node(0).ok_or_else(|| arity(name))?;
            named(&text(1).unwrap_or_default(), child).map_err(failure)
        }
        ("CharClass" | "NotCharClass", 1) => Ok(pattern(Node::CharClass {
            chars: text(0).unwrap_or_default(),
            negated: name == "NotCharClass",
        })),
        ("CharRange", 2) => {
            let (start, end) = (text(0).unwrap_or_default(), text(1).unwrap_or_default());
            let (start, end) = characters(&start, &end).map_err(failure)?;
            Ok(pattern(Node::CharRange(start, end)))
        }
        ("MultiRange", 1) => multi_range(&arguments[0]),
        ("AnyChar", 0) => Ok(pattern(Node::AnyChar)),
        ("Digit", 0) => Ok(pattern(Node::Digit)),
        ("WordChar", 0) => Ok(pattern(Node::WordChar)),
        ("Whitespace", 0) => Ok(pattern(Node::Whitespace)),
        ("StartOfLine", 0) => Ok(pattern(Node::StartOfLine)),
        ("EndOfLine", 0) => Ok(pattern(Node::EndOfLine)),
        ("StartOfString", 0) => Ok(pattern(Node::StartOfString)),
        ("EndOfString", 0) => Ok(pattern(Node::EndOfString)),
        ("DNASequence", 0) => sequence("ACGTacgt"),
        ("StrictDNASequence", 0) => sequence("ACGT"),
        ("DNASequenceWithAmbiguity", 0) => sequence("ACGTRYSWKMBDHVNacgtryswkmbdhvn"),
        ("ProteinSequence", 0) => sequence("ACDEFGHIKLMNPQRSTVWYacdefghiklmnpqrstvwy"),
        ("StrictProteinSequence", 0) => sequence("ACDEFGHIKLMNPQRSTVWY"),
        ("ProteinSequenceWithAmbiguity", 0) => {
            sequence("ACDEFGHIKLMNPQRSTVWYBZXacdefghiklmnpqrstvwybzx*")
        }
        ("PhredQuality", 0) => raw("[!-~]+"),
        ("Chromosome", 0) => raw("(?:chr)?(?:2[0-2]|1[0-9]|[1-9]|[XYMxym])"),
        ("StrictChromosome", 0) => raw("chr(?:2[0-2]|1[0-9]|[1-9]|[XYMxym])"),
        ("ReadPair", 0) => {
            let items = ["R1", "R2", "1", "2"].map(String::from).to_vec();
            Ok(pattern(Node::Either(items)))
        }
        ("FastqExtension", 0) => raw("(?i)\\.(fastq|fq)(\\.(gz))?"),
        ("VcfExtension", 0) => raw("(?i)\\.(vcf|bcf)(\\.(gz))?"),
        ("AlignmentExtension", 0) => raw("(?i)\\.(bam|sam|cram)"),
        ("GffGtfExtension", 0) => raw("(?i)\\.(gff3?|gtf)(\\.(gz))?"),
        ("FastaExtension", 0) => raw("(?i)\\.(fa|fasta|fna)(\\.(gz))?"),
        ("BedExtension", 0) => Ok(pattern(bed_extension())),
        _ => Err(arity(name)),
    }
}

/// `.bed` with an optional `.gz` in any case, built from pairs as `.then` builds them.
fn bed_extension() -> Node {
    let pair = |a: Rc<Node>, b: Rc<Node>| Rc::new(Node::Sequence(vec![a, b]));
    let class = |chars: &str| {
        Rc::new(Node::CharClass {
            chars: chars.into(),
            negated: false,
        })
    };
    let dot = || Rc::new(Node::Literal(".".into()));
    let bed = pair(pair(class("Bb"), class("Ee")), class("Dd"));
    let gz = pair(pair(dot(), class("Gg")), class("Zz"));
    Node::Sequence(vec![pair(dot(), bed), Rc::new(Node::Optional(gz))])
}

/// The patterns of a list, which must not be empty and hold only patterns.
fn nodes(list: &Value, empty: &str) -> Result<Vec<Rc<Node>>, Error> {
    let items = elements(list);
    if items.is_empty() {
        return Err(failure(empty.into()));
    }
    let mut nodes = Vec::new();
    for item in &items {
        match node_of(item) {
            Some(node) => nodes.push(node),
            None if empty.starts_with("AnyOf") => {
                return Err(failure(format!(
                    "AnyOf requires PRegEx patterns; got {}. Use Either(...) for literal strings.",
                    item.java_class_name()
                )));
            }
            None => return Err(arity("Sequence")),
        }
    }
    Ok(nodes)
}

fn either(list: &Value) -> Result<Value, Error> {
    let items = elements(list);
    if items.is_empty() {
        return Err(failure("Either requires at least one alternative".into()));
    }
    if items.iter().any(|item| node_of(item).is_some()) {
        return Err(failure(
            "Either alternates over literal strings, but got a PRegEx pattern. Use AnyOf(...) \
             to alternate over PRegEx sub-patterns."
                .into(),
        ));
    }
    Ok(pattern(Node::Either(
        items.iter().map(Value::to_string).collect(),
    )))
}

fn elements(list: &Value) -> Vec<Value> {
    match list {
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set.borrow().items.iter().map(|key| key.0.clone()).collect(),
        _ => Vec::new(),
    }
}

fn quantified(name: &str, child: Rc<Node>) -> Value {
    pattern(match name {
        "Optional" | "optional" => Node::Optional(child),
        "OneOrMore" | "oneOrMore" => Node::OneOrMore(child),
        "ZeroOrMore" | "zeroOrMore" => Node::ZeroOrMore(child),
        _ => Node::Group(child),
    })
}

/// `Exactly`, `AtLeast` or, with a maximum, `Range`, with the counts checked.
fn counted(name: &str, child: Rc<Node>, min: i64, max: Option<i64>) -> Result<Value, String> {
    Ok(pattern(match (name.to_ascii_lowercase().as_str(), max) {
        ("exactly", _) if min < 0 => return Err("Count must be non-negative".into()),
        ("exactly", _) => Node::Exactly(child, min),
        ("atleast", _) if min < 0 => return Err("Minimum must be non-negative".into()),
        ("atleast", _) => Node::AtLeast(child, min),
        (_, Some(max)) if min < 0 || max < min => {
            return Err(format!("Invalid range: min={min}, max={max}"));
        }
        (_, max) => Node::Range(child, min, max.unwrap_or(min)),
    }))
}

fn named(name: &str, child: Rc<Node>) -> Result<Value, String> {
    let mut characters = name.chars();
    let valid = characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err(
            "Group name must start with a letter and contain only alphanumeric \
                    characters (no underscores)"
                .into(),
        );
    }
    Ok(pattern(Node::NamedGroup(name.into(), child)))
}

/// The two characters of a range, each one UTF-16 unit, the start not after the end.
fn characters(start: &str, end: &str) -> Result<(char, char), String> {
    let single = |text: &str| {
        let mut units = text.encode_utf16();
        match (units.next(), units.next()) {
            (Some(unit), None) => char::from_u32(u32::from(unit)),
            _ => None,
        }
    };
    let first = single(start).ok_or("Start must be a single character string")?;
    let last = single(end).ok_or("End must be a single character string")?;
    if first > last {
        return Err(format!(
            "Start character '{first}' must be less than or equal to end character '{last}'"
        ));
    }
    Ok((first, last))
}

/// The ranges `'a'-'z'` of a specification, with any quote and spaces around the `-`.
fn multi_range(spec: &Value) -> Result<Value, Error> {
    let spec = match spec {
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if spec.is_empty() {
        return Err(failure(
            "Range specification cannot be null or empty".into(),
        ));
    }
    let finder = Pattern::new(r#"['"](.)['"]\s*-\s*['"](.)['"]"#).map_err(thrown)?;
    let mut ranges = Vec::new();
    for groups in finder.all_groups(&spec).map_err(thrown)? {
        let part = |index: usize| groups.get(index).cloned().flatten().unwrap_or_default();
        ranges.push(characters(&part(1), &part(2)).map_err(failure)?);
    }
    if ranges.is_empty() {
        return Err(failure("At least one valid range is required".into()));
    }
    Ok(pattern(Node::MultiRange(ranges)))
}

/// The `IllegalArgumentException` of a check of the plugin.
fn failure(message: String) -> Error {
    thrown(Exception::illegal_argument(message))
}

fn arity(name: &str) -> Error {
    Error::Unsupported {
        message: format!("the function `{name}` of the plugin `nf-pregex` with these arguments"),
        span: Default::default(),
    }
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
