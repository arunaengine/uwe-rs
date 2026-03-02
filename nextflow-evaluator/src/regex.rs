//! Regular expressions with the rules of `java.util.regex` for matching, splitting and replacing.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use fancy_regex::{Captures, Regex};

use crate::exception::Exception;

/// A compiled pattern with its Java source text.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub source: String,
    regex: Regex,
    whole: Regex,
}

impl Pattern {
    /// Compiles a Java pattern. Invalid syntax is Java's `PatternSyntaxException`.
    pub fn new(source: &str) -> Result<Self, Exception> {
        let compile = |text: &str| {
            Regex::new(text).map_err(|error| {
                Exception::new(
                    "PatternSyntaxException",
                    format!("{error} near index 0\n{source}"),
                )
            })
        };
        Ok(Self {
            source: source.to_owned(),
            regex: compile(source)?,
            whole: compile(&format!(r"\A(?:{source})\z"))?,
        })
    }

    /// Whether the pattern matches somewhere in the text, as `find()` does.
    pub fn find(&self, text: &str) -> bool {
        self.regex.is_match(text).unwrap_or(false)
    }

    /// Whether the pattern matches the whole text, as `matches()` does.
    pub fn matches(&self, text: &str) -> bool {
        self.whole.is_match(text).unwrap_or(false)
    }

    /// Every match with its groups: the whole match first, then each group or `None`.
    pub fn all_groups(&self, text: &str) -> Vec<Vec<Option<String>>> {
        self.regex
            .captures_iter(text)
            .filter_map(Result::ok)
            .map(|captures| {
                captures
                    .iter()
                    .map(|group| group.map(|group| group.as_str().to_owned()))
                    .collect()
            })
            .collect()
    }

    /// The number of groups, without the whole match.
    pub fn group_count(&self) -> usize {
        self.regex.captures_len() - 1
    }

    /// Java's `String.split(regex, limit)`. With a limit of 0, trailing empty strings are removed.
    /// A zero-width match at the start never gives an empty first string.
    pub fn split(&self, text: &str, limit: i32) -> Vec<String> {
        let mut parts = Vec::new();
        let mut start = 0;
        for found in self.regex.find_iter(text).filter_map(Result::ok) {
            if limit > 0 && parts.len() as i32 == limit - 1 {
                break;
            }
            if found.end() == 0 {
                continue;
            }
            parts.push(text[start..found.start()].to_owned());
            start = found.end();
        }
        if parts.is_empty() && start == 0 {
            return vec![text.to_owned()];
        }
        parts.push(text[start..].to_owned());
        if limit == 0 {
            while parts.last().is_some_and(String::is_empty) {
                parts.pop();
            }
        }
        parts
    }

    /// Java's `replaceAll` or `replaceFirst` with a replacement string, where `$1` and `${name}`
    /// insert groups and a backslash escapes the next character.
    pub fn replace(&self, text: &str, replacement: &str, all: bool) -> Result<String, Exception> {
        let mut result = String::new();
        let mut last = 0;
        for captures in self.regex.captures_iter(text).filter_map(Result::ok) {
            let whole = captures.get(0).expect("group 0 is the match");
            result.push_str(&text[last..whole.start()]);
            expand(&captures, replacement, &mut result)?;
            last = whole.end();
            if !all {
                break;
            }
        }
        result.push_str(&text[last..]);
        Ok(result)
    }

    /// Calls `replace` for every match with the groups, as Groovy's `replaceAll(regex) { }` does.
    pub fn replace_with<E>(
        &self,
        text: &str,
        all: bool,
        mut replace: impl FnMut(Vec<Option<String>>) -> Result<String, E>,
    ) -> Result<String, E> {
        let mut result = String::new();
        let mut last = 0;
        for captures in self.regex.captures_iter(text).filter_map(Result::ok) {
            let whole = captures.get(0).expect("group 0 is the match");
            result.push_str(&text[last..whole.start()]);
            let groups = captures
                .iter()
                .map(|group| group.map(|group| group.as_str().to_owned()))
                .collect();
            result.push_str(&replace(groups)?);
            last = whole.end();
            if !all {
                break;
            }
        }
        result.push_str(&text[last..]);
        Ok(result)
    }
}

/// Appends a Java replacement string with the groups of one match.
fn expand(
    captures: &Captures<'_, str>,
    replacement: &str,
    out: &mut String,
) -> Result<(), Exception> {
    let illegal = |message: &str| Exception::illegal_argument(message.to_owned());
    let mut characters = replacement.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => match characters.next() {
                Some(escaped) => out.push(escaped),
                None => return Err(illegal("character to be escaped is missing")),
            },
            '$' => {
                let group = match characters.next() {
                    Some('{') => {
                        let name: String = characters.by_ref().take_while(|c| *c != '}').collect();
                        captures
                            .name(&name)
                            .ok_or_else(|| illegal(&format!("No group with name {{{name}}}")))?
                            .as_str()
                            .to_owned()
                    }
                    Some(digit) if digit.is_ascii_digit() => {
                        // Java takes further digits while the number stays a valid group.
                        let mut number = digit.to_digit(10).unwrap_or(0) as usize;
                        if number >= captures.len() {
                            return Err(Exception::new(
                                "IndexOutOfBoundsException",
                                format!("No group {number}"),
                            ));
                        }
                        while let Some(next) = characters.peek().and_then(|c| c.to_digit(10)) {
                            let longer = number * 10 + next as usize;
                            if longer >= captures.len() {
                                break;
                            }
                            number = longer;
                            characters.next();
                        }
                        captures
                            .get(number)
                            .map(|group| group.as_str().to_owned())
                            .unwrap_or_default()
                    }
                    _ => return Err(illegal("Illegal group reference")),
                };
                out.push_str(&group);
            }
            other => out.push(other),
        }
    }
    Ok(())
}
