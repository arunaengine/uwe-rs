//! `re`: Python regular expressions on fancy-regex, with match objects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fancy_regex::{Captures, Regex};

use super::function;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Native, Value};

const IGNORECASE: i64 = 2;
const MULTILINE: i64 = 8;
const DOTALL: i64 = 16;
const VERBOSE: i64 = 64;
const ASCII: i64 = 256;

/// A compiled pattern: the search form and the anchored forms for `match` and `fullmatch`.
pub struct Pattern {
    pub source: String,
    pub flags: i64,
    search: Regex,
    start: Regex,
    whole: Regex,
    names: Vec<Option<String>>,
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, i64), Rc<Pattern>>> = RefCell::new(HashMap::new());
}

/// Rewrites Python syntax that fancy-regex reads differently: `\Z`, and braces that are
/// not repetition counts.
fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut output = String::with_capacity(pattern.len());
    let mut index = 0;
    let mut in_class = false;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '\\' => {
                let next = chars.get(index + 1).copied();
                match next {
                    Some('Z') => output.push_str("\\z"),
                    Some(next) => {
                        output.push('\\');
                        output.push(next);
                    }
                    None => output.push('\\'),
                }
                index += 2;
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                output.push(c);
                // A `]` right after the opening bracket is a literal.
                if chars.get(index + 1) == Some(&'^') {
                    output.push('^');
                    index += 1;
                }
                if chars.get(index + 1) == Some(&']') {
                    output.push_str("\\]");
                    index += 1;
                }
            }
            '[' if in_class => output.push_str("\\["),
            ']' if in_class => {
                in_class = false;
                output.push(c);
            }
            '{' if !in_class => {
                // A repetition count is copied whole; any other brace is literal text.
                let end = chars[index + 1..]
                    .iter()
                    .position(|c| *c == '}')
                    .map(|end| index + 1 + end);
                let quantifier = end.filter(|end| {
                    let inner: String = chars[index + 1..*end].iter().collect();
                    !inner.is_empty()
                        && inner != ","
                        && inner.chars().all(|c| c.is_ascii_digit() || c == ',')
                        && inner.matches(',').count() <= 1
                        && !output.is_empty()
                });
                match quantifier {
                    Some(end) => {
                        output.extend(&chars[index..=end]);
                        index = end + 1;
                        continue;
                    }
                    None => output.push_str("\\{"),
                }
            }
            '}' if !in_class => output.push_str("\\}"),
            other => output.push(other),
        }
        index += 1;
    }
    output
}

/// Compiles a Python pattern with `re` flags; patterns are cached.
pub fn compile(pattern: &str, flags: i64) -> Result<Rc<Pattern>, Error> {
    if let Some(found) =
        CACHE.with(|cache| cache.borrow().get(&(pattern.to_owned(), flags)).cloned())
    {
        return Ok(found);
    }
    let mut prefix = String::new();
    if flags & IGNORECASE != 0 {
        prefix.push('i');
    }
    if flags & MULTILINE != 0 {
        prefix.push('m');
    }
    if flags & DOTALL != 0 {
        prefix.push('s');
    }
    if flags & VERBOSE != 0 {
        prefix.push('x');
    }
    let translated = translate(pattern);
    let body = if prefix.is_empty() {
        translated
    } else {
        format!("(?{prefix}){translated}")
    };
    let error = |error: fancy_regex::Error| {
        crate::types::library_exception("re.error", "re", "Exception");
        Error::new(
            "re.error",
            format!("{error} in pattern {}", crate::format::quote(pattern)),
        )
    };
    let search = Regex::new(&body).map_err(error)?;
    let start = Regex::new(&format!("\\A(?:{body})")).map_err(error)?;
    let whole = Regex::new(&format!("\\A(?:{body})\\z")).map_err(error)?;
    let names = search
        .capture_names()
        .map(|name| name.map(str::to_owned))
        .collect();
    let compiled = Rc::new(Pattern {
        source: pattern.to_owned(),
        flags,
        search,
        start,
        whole,
        names,
    });
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert((pattern.to_owned(), flags), compiled.clone())
    });
    Ok(compiled)
}

impl Pattern {
    pub fn fullmatch(&self, text: &str) -> bool {
        self.whole.is_match(text).unwrap_or(false)
    }

    pub fn search_regex(&self) -> &Regex {
        &self.search
    }

    /// Named groups of a full match, or `None` when the text does not match.
    pub fn full_captures(&self, text: &str) -> Option<Vec<(String, String)>> {
        let captures = self.whole.captures(text).ok()??;
        Some(
            self.names
                .iter()
                .enumerate()
                .filter_map(|(index, name)| {
                    let name = name.as_ref()?;
                    let group = captures.get(index)?;
                    Some((name.clone(), group.as_str().to_owned()))
                })
                .collect(),
        )
    }

    fn group_index(&self, key: &Value) -> Result<usize, Error> {
        match key.plain() {
            Value::Int(index) if *index >= 0 && (*index as usize) < self.names.len() => {
                Ok(*index as usize)
            }
            Value::Str(name) => self
                .names
                .iter()
                .position(|candidate| candidate.as_deref() == Some(&**name))
                .ok_or_else(|| Error::index_error("no such group")),
            _ => Err(Error::index_error("no such group")),
        }
    }
}

fn char_offset(text: &str, byte: usize) -> usize {
    if text.is_ascii() {
        byte
    } else {
        text[..byte].chars().count()
    }
}

/// A match object.
pub struct Match {
    pattern: Rc<Pattern>,
    text: Rc<str>,
    /// Byte ranges of the groups, the whole match first.
    groups: Vec<Option<(usize, usize)>>,
}

impl Match {
    fn new(pattern: Rc<Pattern>, text: Rc<str>, captures: &Captures<'_, str>) -> Match {
        let groups = (0..captures.len())
            .map(|index| {
                captures
                    .get(index)
                    .map(|group| (group.start(), group.end()))
            })
            .collect();
        Match {
            pattern,
            text,
            groups,
        }
    }

    fn group(&self, index: usize) -> Value {
        match self.groups.get(index).copied().flatten() {
            Some((start, end)) => Value::str(&self.text[start..end]),
            None => Value::None,
        }
    }

    fn group_value(&self, key: &Value) -> Result<Value, Error> {
        let index = self.pattern.group_index(key)?;
        Ok(self.group(index))
    }
}

impl Native for Match {
    fn type_name(&self) -> &str {
        "re.Match"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn truthy(&self) -> Option<bool> {
        Some(true)
    }

    fn get_item(
        &self,
        _: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        self.group_value(key).map(Some)
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        let (start, end) = self.groups[0].unwrap_or((0, 0));
        Ok(format!(
            "<re.Match object; span=({}, {}), match={}>",
            char_offset(&self.text, start),
            char_offset(&self.text, end),
            crate::format::quote(&self.text[start..end])
        ))
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        let method = |name: &'static str, body: fn(&Match, Args) -> Result<Value, Error>| {
            let this = this.clone();
            Value::builtin(name, move |_, args| {
                let found = this.as_native::<Match>().expect("a match");
                body(found, args)
            })
        };
        Ok(Some(match name {
            "string" => Value::Str(self.text.clone()),
            "re" => Value::native(PatternObject(self.pattern.clone())),
            "pos" => Value::Int(0),
            "lastindex" => {
                let last = self
                    .groups
                    .iter()
                    .enumerate()
                    .skip(1)
                    .rev()
                    .find(|(_, group)| group.is_some());
                last.map_or(Value::None, |(index, _)| Value::Int(index as i64))
            }
            "group" => method("group", |found, args| {
                if args.positional.is_empty() {
                    return Ok(found.group(0));
                }
                if args.positional.len() == 1 {
                    return found.group_value(&args.positional[0]);
                }
                let groups = args
                    .positional
                    .iter()
                    .map(|key| found.group_value(key))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::tuple(groups))
            }),
            "groups" => method("groups", |found, mut args| {
                let default = args.take(0, "default").unwrap_or(Value::None);
                Ok(Value::tuple(
                    (1..found.groups.len())
                        .map(|index| {
                            let group = found.group(index);
                            if group.is_none() {
                                default.clone()
                            } else {
                                group
                            }
                        })
                        .collect(),
                ))
            }),
            "groupdict" => method("groupdict", |found, mut args| {
                let default = args.take(0, "default").unwrap_or(Value::None);
                let mut dict = Dict::default();
                for (index, name) in found.pattern.names.iter().enumerate() {
                    if let Some(name) = name {
                        let group = found.group(index);
                        dict.insert(
                            name,
                            if group.is_none() {
                                default.clone()
                            } else {
                                group
                            },
                        );
                    }
                }
                Ok(Value::dict(dict))
            }),
            "start" | "end" | "span" => {
                let which: &'static str = match name {
                    "start" => "start",
                    "end" => "end",
                    _ => "span",
                };
                let this = this.clone();
                Value::builtin(which, move |_, args| {
                    let found = this.as_native::<Match>().expect("a match");
                    let index = match args.positional.first() {
                        Some(key) => found.pattern.group_index(key)?,
                        None => 0,
                    };
                    let range = found.groups.get(index).copied().flatten();
                    let (start, end) = match range {
                        Some((start, end)) => (
                            char_offset(&found.text, start) as i64,
                            char_offset(&found.text, end) as i64,
                        ),
                        None => (-1, -1),
                    };
                    Ok(match which {
                        "start" => Value::Int(start),
                        "end" => Value::Int(end),
                        _ => Value::tuple(vec![Value::Int(start), Value::Int(end)]),
                    })
                })
            }
            "expand" => {
                let this = this.clone();
                Value::builtin("expand", move |_, args| {
                    let found = this.as_native::<Match>().expect("a match");
                    let template = args
                        .positional
                        .first()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    Ok(Value::str(expand_template(&template, found)?))
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// Expands `\1`, `\g<name>` and escapes of a replacement template.
fn expand_template(template: &str, found: &Match) -> Result<String, Error> {
    let chars: Vec<char> = template.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c != '\\' || index + 1 >= chars.len() {
            output.push(c);
            index += 1;
            continue;
        }
        let next = chars[index + 1];
        index += 2;
        match next {
            'g' if chars.get(index) == Some(&'<') => {
                let end = chars[index..]
                    .iter()
                    .position(|c| *c == '>')
                    .map(|end| end + index);
                let Some(end) = end else {
                    return Err(Error::new("re.error", "missing >, unterminated name"));
                };
                let name: String = chars[index + 1..end].iter().collect();
                index = end + 1;
                let key = match name.parse::<i64>() {
                    Ok(number) => Value::Int(number),
                    Err(_) => Value::str(name),
                };
                let group = found.group_value(&key)?;
                output.push_str(group.as_str().unwrap_or(""));
            }
            d if d.is_ascii_digit() => {
                let mut number = d.to_digit(10).unwrap_or(0) as i64;
                if let Some(second) = chars.get(index).and_then(|c| c.to_digit(10)) {
                    number = number * 10 + second as i64;
                    index += 1;
                }
                let group = found.group_value(&Value::Int(number))?;
                output.push_str(group.as_str().unwrap_or(""));
            }
            'n' => output.push('\n'),
            't' => output.push('\t'),
            'r' => output.push('\r'),
            '\\' => output.push('\\'),
            other => {
                output.push('\\');
                output.push(other);
            }
        }
    }
    Ok(output)
}

/// A compiled pattern as a Python object.
pub struct PatternObject(pub Rc<Pattern>);
