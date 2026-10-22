//! Regular expressions with the rules of `java.util.regex` for matching, splitting and replacing.
// Copyright (c) 2026 The uwe Contributors
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
        let translated = classes(&unquoted(source)).map_err(|error| {
            Exception::new(
                "PatternSyntaxException",
                format!("{error} near index 0\n{source}"),
            )
        })?;
        Ok(Self {
            source: source.to_owned(),
            regex: compile(&translated)?,
            whole: compile(&format!(r"\A(?:{translated})\z"))?,
        })
    }

    /// Whether the pattern matches somewhere in the text, as `find()` does.
    pub fn find(&self, text: &str) -> Result<bool, Exception> {
        self.regex.is_match(text).map_err(failed)
    }

    /// Whether the pattern matches the whole text, as `matches()` does.
    pub fn matches(&self, text: &str) -> Result<bool, Exception> {
        self.whole.is_match(text).map_err(failed)
    }

    /// Every match with its groups: the whole match first, then each group or `None`.
    pub fn all_groups(&self, text: &str) -> Result<Vec<Vec<Option<String>>>, Exception> {
        self.regex
            .captures_iter(text)
            .map(|captures| captures.map(|captures| groups(&captures)).map_err(failed))
            .collect()
    }

    pub fn count(&self, text: &str) -> Result<usize, Exception> {
        self.regex
            .find_iter(text)
            .try_fold(0, |count, found| found.map(|_| count + 1).map_err(failed))
    }

    pub fn first_groups(&self, text: &str) -> Result<Option<Vec<Option<String>>>, Exception> {
        self.regex
            .captures(text)
            .map(|captures| captures.as_ref().map(groups))
            .map_err(failed)
    }

    /// The number of groups, without the whole match.
    pub fn group_count(&self) -> usize {
        self.regex.captures_len() - 1
    }

    /// Java's `String.split(regex, limit)`. With a limit of 0, trailing empty strings are removed.
    /// A zero-width match at the start never gives an empty first string.
    pub fn split(&self, text: &str, limit: i32) -> Result<Vec<String>, Exception> {
        let mut parts = Vec::new();
        let mut start = 0;
        for found in self.regex.find_iter(text) {
            let found = found.map_err(failed)?;
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
            return Ok(vec![text.to_owned()]);
        }
        parts.push(text[start..].to_owned());
        if limit == 0 {
            while parts.last().is_some_and(String::is_empty) {
                parts.pop();
            }
        }
        Ok(parts)
    }

    /// Java's `replaceAll` or `replaceFirst` with a replacement string, where `$1` and `${name}`
    /// insert groups and a backslash escapes the next character.
    pub fn replace(&self, text: &str, replacement: &str, all: bool) -> Result<String, Exception> {
        let mut result = String::new();
        let mut last = 0;
        for captures in self.regex.captures_iter(text) {
            let captures = captures.map_err(failed)?;
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
        error: impl Fn(Exception) -> E,
        mut replace: impl FnMut(Vec<Option<String>>) -> Result<String, E>,
    ) -> Result<String, E> {
        let mut result = String::new();
        let mut last = 0;
        for captures in self.regex.captures_iter(text) {
            let captures = captures.map_err(|cause| error(failed(cause)))?;
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

/// A matcher retaining only its first and current match.
#[derive(Debug)]
pub struct Matcher {
    pub pattern: std::rc::Rc<Pattern>,
    pub text: String,
    pub first: Option<Vec<Option<String>>>,
    pub current: std::cell::RefCell<Option<Vec<Option<String>>>>,
    cursor: std::cell::Cell<Option<usize>>,
}

impl Matcher {
    pub fn new(pattern: std::rc::Rc<Pattern>, text: String) -> Result<Self, Exception> {
        let first = pattern.first_groups(&text)?;
        Ok(Self {
            pattern,
            text,
            first,
            current: std::cell::RefCell::new(None),
            cursor: std::cell::Cell::new(Some(0)),
        })
    }

    pub fn find(&self) -> Result<bool, Exception> {
        let found = match self.cursor.get() {
            Some(cursor) => self
                .pattern
                .regex
                .captures_from_pos(self.text.as_str(), cursor)
                .map_err(failed)?,
            None => None,
        };
        let next = found.as_ref().and_then(|captures| {
            let whole = captures.get(0).expect("group 0 is the match");
            if whole.start() == whole.end() {
                self.text[whole.end()..]
                    .chars()
                    .next()
                    .map(|c| whole.end() + c.len_utf8())
            } else {
                Some(whole.end())
            }
        });
        self.cursor.set(next);
        *self.current.borrow_mut() = found.as_ref().map(groups);
        Ok(found.is_some())
    }
}

fn groups(captures: &Captures<'_, str>) -> Vec<Option<String>> {
    captures
        .iter()
        .map(|group| group.map(|group| group.as_str().to_owned()))
        .collect()
}

fn failed(error: fancy_regex::Error) -> Exception {
    Exception::new(
        "IllegalStateException",
        format!("Regex execution failed: {error}"),
    )
}

/// Java enables Unicode character classes with U, independently of its Unicode case flag u.
fn classes(source: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut characters = source.chars().peekable();
    let mut unicode = false;
    let mut folding = false;
    let mut insensitive = false;
    let mut scopes = Vec::new();
    let mut depth = 0usize;
    let mut class_start = 0;
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                let next = characters.next().unwrap_or('\\');
                let replacement = match (unicode, next) {
                    (false, 'd') => Some("[0-9]"),
                    (false, 'D') => Some("[^0-9]"),
                    (false, 'w') => Some("[a-zA-Z_0-9]"),
                    (false, 'W') => Some("[^a-zA-Z_0-9]"),
                    (false, 's') => Some(r"[ \t\n\x0B\f\r]"),
                    (false, 'S') => Some(r"[^ \t\n\x0B\f\r]"),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    if depth > 0 && insensitive {
                        result.push('\\');
                        result.push(next);
                    } else {
                        result.push_str(replacement);
                    }
                } else {
                    let start = result.len();
                    result.push('\\');
                    result.push(next);
                    if depth == 0 && (next.is_ascii_digit() || next == 'k') {
                        if next == 'k' {
                            for next in characters.by_ref() {
                                result.push(next);
                                if next == '>' {
                                    break;
                                }
                            }
                        } else {
                            while characters.peek().is_some_and(char::is_ascii_digit) {
                                result.push(characters.next().expect("reference digit"));
                            }
                        }
                        if insensitive {
                            let reference = format!("(?i:{})", &result[start..]);
                            result.truncate(start);
                            result.push_str(&reference);
                        }
                    } else if matches!(next, 'x' | 'u' | 'p' | 'P') {
                        if characters.peek() == Some(&'{') {
                            for next in characters.by_ref() {
                                result.push(next);
                                if next == '}' {
                                    break;
                                }
                            }
                        } else {
                            let count = match next {
                                'x' => 2,
                                'u' => 4,
                                _ => 1,
                            };
                            result.extend(characters.by_ref().take(count));
                        }
                        if depth == 0 && insensitive {
                            let atom =
                                case_class(&format!("[{}]", &result[start..]), unicode, folding)?;
                            result.truncate(start);
                            result.push_str(&atom);
                        }
                    }
                }
            }
            '[' => {
                if depth == 0 {
                    class_start = result.len();
                }
                depth += 1;
                result.push(character);
            }
            ']' if depth > 0 => {
                depth = depth.saturating_sub(1);
                result.push(character);
                if depth == 0 && insensitive {
                    let class = case_class(&result[class_start..], unicode, folding)?;
                    result.truncate(class_start);
                    result.push_str(&class);
                }
            }
            '(' if depth == 0 => {
                let mut probe = characters.clone();
                let flags = if probe.next() == Some('?') {
                    probe
                        .by_ref()
                        .take_while(|c| matches!(c, 'i' | 'm' | 's' | 'x' | 'u' | 'U' | '-'))
                        .collect::<String>()
                } else {
                    String::new()
                };
                let end = characters.clone().nth(flags.len() + 1);
                if !flags.is_empty() && matches!(end, Some(')' | ':')) {
                    let outer = (unicode, folding, insensitive);
                    let mut enable = true;
                    let mut kept = String::new();
                    for flag in flags.chars() {
                        match flag {
                            '-' => {
                                enable = false;
                                kept.push('-');
                            }
                            'U' => {
                                unicode = enable;
                                folding = enable;
                            }
                            'u' => folding = enable,
                            'i' => insensitive = enable,
                            other => kept.push(other),
                        }
                    }
                    for _ in 0..flags.len() + 2 {
                        characters.next();
                    }
                    let kept = kept.trim_end_matches('-');
                    let case = "(?-i)";
                    if end == Some(':') {
                        scopes.push(outer);
                        result.push_str("(?");
                        result.push_str(kept);
                        result.push(':');
                        result.push_str(case);
                    } else if !kept.is_empty() {
                        result.push_str("(?");
                        result.push_str(kept);
                        result.push(')');
                        result.push_str(case);
                    } else {
                        result.push_str(case);
                    }
                } else {
                    scopes.push((unicode, folding, insensitive));
                    result.push(character);
                    if characters.peek() == Some(&'?') {
                        let mut probe = characters.clone();
                        probe.next();
                        if probe.next() == Some('<') && !matches!(probe.next(), Some('=' | '!')) {
                            for next in characters.by_ref() {
                                result.push(next);
                                if next == '>' {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            ')' if depth == 0 => {
                (unicode, folding, insensitive) =
                    scopes.pop().unwrap_or((unicode, folding, insensitive));
                result.push(character);
            }
            other if depth == 0 && insensitive && other.is_alphabetic() => {
                if folding {
                    if matches!(other, 'i' | 'I' | 'ı' | 'İ') {
                        result.push_str("[iIıİ]");
                    } else {
                        result.push_str(&format!("(?i:{other})"));
                    }
                } else if other.is_ascii_alphabetic() {
                    result.push('[');
                    result.push(other.to_ascii_lowercase());
                    result.push(other.to_ascii_uppercase());
                    result.push(']');
                } else {
                    result.push(other);
                }
            }
            other => result.push(other),
        }
    }
    Ok(result)
}

fn case_class(source: &str, unicode: bool, folding: bool) -> Result<String, String> {
    use regex_syntax::ast::{Ast, ClassBracketed, ClassSet, ClassSetItem};
    fn fold(set: &ClassSet, unicode: bool, folding: bool) -> Result<String, String> {
        let wrap = |set: ClassSet| {
            Ast::ClassBracketed(Box::new(ClassBracketed {
                span: *set.span(),
                negated: false,
                kind: set,
            }))
            .to_string()
        };
        match set {
            ClassSet::BinaryOp(op) => {
                let sign = match op.kind {
                    regex_syntax::ast::ClassSetBinaryOpKind::Intersection => "&&",
                    regex_syntax::ast::ClassSetBinaryOpKind::Difference => "--",
                    regex_syntax::ast::ClassSetBinaryOpKind::SymmetricDifference => "~~",
                };
                Ok(format!(
                    "[{}{sign}{}]",
                    fold(&op.lhs, unicode, folding)?,
                    fold(&op.rhs, unicode, folding)?
                ))
            }
            ClassSet::Item(ClassSetItem::Bracketed(class)) => bracket(class, unicode, folding),
            ClassSet::Item(ClassSetItem::Perl(class)) => {
                use regex_syntax::ast::ClassPerlKind;
                let source = if unicode {
                    wrap(set.clone())
                } else {
                    let body = match class.kind {
                        ClassPerlKind::Digit => "0-9",
                        ClassPerlKind::Space => r" \t\n\x0B\f\r",
                        ClassPerlKind::Word => "a-zA-Z_0-9",
                    };
                    format!("[{}{body}]", if class.negated { "^" } else { "" })
                };
                Ok(source)
            }
            ClassSet::Item(ClassSetItem::Union(union)) => {
                let parts = union
                    .items
                    .iter()
                    .map(|item| fold(&ClassSet::Item(item.clone()), unicode, folding))
                    .collect::<Result<String, _>>()?;
                Ok(format!("[{parts}]"))
            }
            _ => {
                let mut set = set.clone();
                let negated = match &mut set {
                    ClassSet::Item(ClassSetItem::Unicode(class)) => {
                        std::mem::take(&mut class.negated)
                    }
                    ClassSet::Item(ClassSetItem::Perl(class)) => std::mem::take(&mut class.negated),
                    ClassSet::Item(ClassSetItem::Ascii(class)) => {
                        std::mem::take(&mut class.negated)
                    }
                    _ => false,
                };
                let source = wrap(set);
                let hir = regex_syntax::parse(&source).map_err(|error| error.to_string())?;
                if folding {
                    let mut class = match hir.kind() {
                        regex_syntax::hir::HirKind::Class(regex_syntax::hir::Class::Unicode(
                            class,
                        )) => class.clone(),
                        regex_syntax::hir::HirKind::Literal(literal) => {
                            regex_syntax::hir::ClassUnicode::new(
                                std::str::from_utf8(&literal.0)
                                    .map_err(|error| error.to_string())?
                                    .chars()
                                    .map(|letter| {
                                        regex_syntax::hir::ClassUnicodeRange::new(letter, letter)
                                    }),
                            )
                        }
                        _ => return Ok(source),
                    };
                    class.case_fold_simple();
                    if class.ranges().iter().any(|range| {
                        ['i', 'I', 'ı', 'İ']
                            .iter()
                            .any(|letter| range.start() <= *letter && *letter <= range.end())
                    }) {
                        for letter in ['i', 'I', 'ı', 'İ'] {
                            class.push(regex_syntax::hir::ClassUnicodeRange::new(letter, letter));
                        }
                    }
                    if negated {
                        class.negate();
                    }
                    return Ok(
                        regex_syntax::hir::Hir::class(regex_syntax::hir::Class::Unicode(class))
                            .to_string(),
                    );
                }
                let contains = |letter: char| match hir.kind() {
                    regex_syntax::hir::HirKind::Class(regex_syntax::hir::Class::Unicode(class)) => {
                        class
                            .ranges()
                            .iter()
                            .any(|range| range.start() <= letter && letter <= range.end())
                    }
                    regex_syntax::hir::HirKind::Literal(literal) => {
                        literal.0.as_ref() == letter.to_string().as_bytes()
                    }
                    _ => false,
                };
                let mut letters = String::new();
                for lower in 'a'..='z' {
                    let upper = lower.to_ascii_uppercase();
                    if contains(lower) || contains(upper) {
                        letters.push(lower);
                        letters.push(upper);
                    }
                }
                Ok(format!(
                    "[{}{source}{letters}]",
                    if negated { "^" } else { "" }
                ))
            }
        }
    }
    fn bracket(class: &ClassBracketed, unicode: bool, folding: bool) -> Result<String, String> {
        Ok(format!(
            "[{}{}]",
            if class.negated { "^" } else { "" },
            fold(&class.kind, unicode, folding)?
        ))
    }
    match regex_syntax::ast::parse::Parser::new()
        .parse(source)
        .map_err(|error| error.to_string())?
    {
        Ast::ClassBracketed(ref class) => bracket(class, unicode, folding),
        _ => Ok(source.to_owned()),
    }
}

/// Java's `\Q...\E` quotes as escaped text, which the Rust engine does not know.
fn unquoted(source: &str) -> String {
    let mut result = String::new();
    let mut rest = source;
    while let Some(start) = rest.find("\\Q") {
        result.push_str(&rest[..start]);
        let quoted = &rest[start + 2..];
        let end = quoted.find("\\E").unwrap_or(quoted.len());
        result.push_str(&fancy_regex::escape(&quoted[..end]));
        rest = quoted.get(end + 2..).unwrap_or("");
    }
    result.push_str(rest);
    result
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
