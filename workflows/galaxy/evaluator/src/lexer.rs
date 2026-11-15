//! Splits a Cheetah template into text, placeholders and directives, with Cheetah's rules for
//! the whitespace and line ends around directives and comments.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Error;

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Text(String),
    /// A placeholder as a Cheetah expression, like `$a.b` or `$x.replace(',', ' ')`.
    Placeholder {
        expression: String,
        line: usize,
    },
    /// A directive with its argument; `short` closes at the end of its line, as `#if a: b`.
    Directive {
        name: String,
        argument: String,
        line: usize,
        short: bool,
    },
}

/// Directive names that Cheetah knows; `#` before any other word is text.
const DIRECTIVES: &[&str] = &[
    "if",
    "elif",
    "else",
    "end",
    "set",
    "silent",
    "echo",
    "for",
    "while",
    "def",
    "block",
    "import",
    "from",
    "raw",
    "slurp",
    "include",
    "filter",
    "break",
    "continue",
    "pass",
    "stop",
    "return",
    "try",
    "except",
    "finally",
    "unless",
    "attr",
    "extends",
    "implements",
    "call",
    "capture",
    "cache",
    "errorCatcher",
    "breakpoint",
    "indent",
    "assert",
    "del",
    "global",
    "yield",
    "encoding",
    "compiler",
    "compiler-settings",
    "transform",
    "py",
];

/// Directives whose argument ends at a colon, which may start a body on the same line.
const BLOCKS: &[&str] = &[
    "if", "elif", "else", "for", "while", "def", "block", "unless",
];

/// Splits `text`, a template whose first line is line 1.
pub fn tokens(text: &str) -> Result<Vec<Token>, Error> {
    let mut lexer = Lexer {
        text,
        at: 0,
        tokens: Vec::new(),
        short: Vec::new(),
    };
    lexer.run()?;
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    text: &'a str,
    at: usize,
    tokens: Vec<Token>,
    /// Ends of short directive bodies: the offset after their line end, and the directive.
    short: Vec<(usize, String)>,
}

impl Lexer<'_> {
    fn run(&mut self) -> Result<(), Error> {
        while self.at < self.text.len() {
            self.close_short();
            let rest = &self.text[self.at..];
            if rest.starts_with("\\$") || rest.starts_with("\\#") {
                self.push_text(&rest[1..2]);
                self.at += 2;
            } else if rest.starts_with('$') && placeholder_start(&rest[1..]) {
                self.placeholder()?;
            } else if rest.starts_with("##") {
                self.comment();
            } else if rest.starts_with("#*") {
                self.block_comment();
            } else if let Some(name) = rest.strip_prefix('#').and_then(directive_name) {
                self.directive(name)?;
            } else {
                let next = rest.chars().next().map_or(1, char::len_utf8);
                self.push_text(&rest[..next]);
                self.at += next;
            }
        }
        self.close_short();
        if let Some((_, name)) = self.short.pop() {
            self.tokens.push(end(name, self.line()));
        }
        Ok(())
    }

    fn line(&self) -> usize {
        1 + self.text[..self.at].matches('\n').count()
    }

    fn close_short(&mut self) {
        while let Some((at, name)) = self.short.last().cloned() {
            if self.at < at {
                break;
            }
            self.short.pop();
            let line = self.line();
            self.tokens.push(end(name, line));
        }
    }

    fn push_text(&mut self, text: &str) {
        match self.tokens.last_mut() {
            Some(Token::Text(last)) => last.push_str(text),
            _ => self.tokens.push(Token::Text(text.to_owned())),
        }
    }

    /// Whether only spaces and tabs come before the current offset on its line.
    fn line_clear(&self) -> bool {
        let start = self.text[..self.at].rfind('\n').map_or(0, |at| at + 1);
        self.text[start..self.at]
            .chars()
            .all(|c| c == ' ' || c == '\t')
    }

    fn end_of_line(&self) -> usize {
        self.text[self.at..]
            .find('\n')
            .map_or(self.text.len(), |at| self.at + at)
    }

    /// Drops the spaces and tabs that start the current line, which Cheetah removes before a
    /// directive alone on its line.
    fn trim_before(&mut self) {
        if let Some(Token::Text(last)) = self.tokens.last_mut() {
            let kept = last.trim_end_matches([' ', '\t']).len();
            last.truncate(kept);
            if last.is_empty() {
                self.tokens.pop();
            }
        }
    }

    /// Moves past the line end at the current offset, `\r\n` included.
    fn eat_line_end(&mut self) {
        let rest = &self.text[self.at..];
        if rest.starts_with("\r\n") {
            self.at += 2;
        } else if rest.starts_with('\n') || rest.starts_with('\r') {
            self.at += 1;
        }
    }

    fn placeholder(&mut self) -> Result<(), Error> {
        let line = self.line();
        let start = self.at + 1;
        let expression = match self.text[start..].chars().next() {
            Some(open @ ('{' | '(' | '[')) => {
                let close = closing(self.text, start, open).ok_or_else(|| {
                    Error::template(
                        line,
                        format!("the placeholder starting with ${open} is not closed"),
                    )
                })?;
                let inside = self.text[start + 1..close].trim();
                self.at = close + 1;
                match inside.chars().next() {
                    Some(c) if c == '_' || c.is_alphabetic() => format!("${inside}"),
                    _ => inside.to_owned(),
                }
            }
            _ => {
                let end = name_chain(self.text, start).ok_or_else(|| {
                    Error::template(line, "a placeholder with an unclosed bracket".to_owned())
                })?;
                self.at = end;
                format!("${}", &self.text[start..end])
            }
        };
        self.tokens.push(Token::Placeholder { expression, line });
        Ok(())
    }

    fn comment(&mut self) {
        let clear = self.line_clear();
        if clear {
            self.trim_before();
        }
        self.at = self.end_of_line();
        if clear {
            self.eat_line_end();
        }
    }

    fn block_comment(&mut self) {
        let clear = self.line_clear();
        let first_end = self.end_of_line();
        let mut depth = 0;
        while self.at < self.text.len() {
            let rest = &self.text[self.at..];
            if rest.starts_with("#*") {
                depth += 1;
                self.at += 2;
            } else if rest.starts_with("*#") {
                depth -= 1;
                self.at += 2;
                if depth == 0 {
                    break;
                }
            } else {
                self.at += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        let eol = self.end_of_line();
        if self.text[self.at..eol].trim().is_empty() {
            self.at = eol;
            if clear {
                self.eat_line_end();
            }
        }
        if clear && (self.at >= self.text.len() || self.at > first_end) {
            self.trim_before();
        }
    }

    fn directive(&mut self, name: &str) -> Result<(), Error> {
        let line = self.line();
        let clear = self.line_clear();
        let first_end = self.end_of_line();
        self.at += 1 + name.len();
        if name == "slurp" {
            if clear {
                self.trim_before();
            }
            self.at = first_end;
            self.eat_line_end();
            return Ok(());
        }
        let colon = BLOCKS.contains(&name);
        let argument_end = argument_end(self.text, self.at, colon);
        let argument = self.text[self.at..argument_end].trim().to_owned();
        self.at = argument_end;
        let mut short = false;
        if colon && self.text[self.at..].starts_with(':') {
            let body = &self.text[self.at + 1..first_end];
            if body.trim().is_empty() || body.trim_start().starts_with("##") {
                self.at += 1;
            } else {
                short = true;
                self.at += 1;
                if self.text[self.at..].starts_with([' ', '\t']) {
                    self.at += 1;
                }
                let after = first_end + usize::from(first_end < self.text.len());
                let block = if name == "elif" || name == "else" {
                    "if"
                } else {
                    name
                };
                self.short.push((after, block.to_owned()));
            }
        }
        if !short && self.rest_of_tag(clear, first_end) {
            self.trim_before();
        }
        self.tokens.push(Token::Directive {
            name: name.to_owned(),
            argument,
            line,
            short,
        });
        if name == "raw" && !short {
            self.raw(line)?;
        }
        Ok(())
    }

    /// Cheetah's `_eatRestOfDirectiveTag`: a closing `#` ends the directive and keeps the
    /// line; alone on its line, it also takes the line end. True when the indent goes too.
    fn rest_of_tag(&mut self, clear: bool, first_end: usize) -> bool {
        while self.text[self.at..].starts_with([' ', '\t']) {
            self.at += 1;
        }
        let rest = &self.text[self.at..];
        if rest.starts_with("##") {
            self.at = self.end_of_line();
            if clear {
                self.eat_line_end();
            }
        } else if rest.starts_with('#') {
            self.at += 1;
        } else if clear && rest.starts_with(['\r', '\n']) {
            self.eat_line_end();
        }
        clear && (self.at >= self.text.len() || self.at > first_end)
    }

    /// The text of a `#raw` block, up to its `#end raw`, which stays for the parser.
    fn raw(&mut self, line: usize) -> Result<(), Error> {
        let Some(found) = self.text[self.at..].find("#end raw") else {
            return Err(Error::template(line, "#raw without #end raw".to_owned()));
        };
        let body = self.text[self.at..self.at + found].to_owned();
        self.at += found;
        if !body.is_empty() {
            self.tokens.push(Token::Text(body));
        }
        if self.line_clear() {
            self.trim_before();
        }
        Ok(())
    }
}

fn end(name: String, line: usize) -> Token {
    Token::Directive {
        name: "end".to_owned(),
        argument: name,
        line,
        short: true,
    }
}

fn placeholder_start(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_alphabetic() || matches!(c, '{' | '(' | '['))
}

fn directive_name(rest: &str) -> Option<&'static str> {
    DIRECTIVES
        .iter()
        .copied()
        .filter(|name| {
            rest.starts_with(name)
                && !rest[name.len()..]
                    .starts_with(|c: char| c == '_' || c == '-' || c.is_alphanumeric())
        })
        .max_by_key(|name| name.len())
}

/// The end of a Cheetah name chain at `start`: an identifier, then `.name`, `(...)` and
/// `[...]` parts; `None` when a bracket is not closed.
pub fn name_chain(text: &str, start: usize) -> Option<usize> {
    let mut at = identifier_end(text, start);
    loop {
        let rest = &text[at..];
        if let Some(after) = rest.strip_prefix('.')
            && after.starts_with(|c: char| c == '_' || c.is_alphabetic())
        {
            at = identifier_end(text, at + 1);
        } else if let Some(open @ ('(' | '[')) = rest.chars().next() {
            at = closing(text, at, open)? + 1;
        } else {
            return Some(at);
        }
    }
}

pub fn identifier_end(text: &str, start: usize) -> usize {
    text[start..]
        .find(|c: char| c != '_' && !c.is_alphanumeric())
        .map_or(text.len(), |at| start + at)
}

/// The offset of the bracket that closes the one at `open_at`, skipping Python strings.
pub fn closing(text: &str, open_at: usize, open: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = open_at;
    while at < text.len() {
        let c = text[at..].chars().next()?;
        match c {
            '\'' | '"' => {
                at = string_end(text, at)?;
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return (matches!((open, c), ('(', ')') | ('[', ']') | ('{', '}')))
                        .then_some(at);
                }
            }
            _ => {}
        }
        at += c.len_utf8();
    }
    None
}

/// The offset after the Python string literal that starts with the quote at `start`.
pub fn string_end(text: &str, start: usize) -> Option<usize> {
    let quote = &text[start..start + 1];
    let triple = quote.repeat(3);
    let (delimiter, mut at) = if text[start..].starts_with(&triple) {
        (triple.as_str(), start + 3)
    } else {
        (quote, start + 1)
    };
    while at < text.len() {
        let rest = &text[at..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            at += 1 + escaped.chars().next().map_or(0, char::len_utf8);
        } else if rest.starts_with(delimiter) {
            return Some(at + delimiter.len());
        } else if delimiter.len() == 1 && rest.starts_with('\n') {
            return None;
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// Where a directive argument ends: the line end, a closing `#`, or with `colon` a colon
/// outside brackets and strings.
fn argument_end(text: &str, start: usize, colon: bool) -> usize {
    let mut depth = 0usize;
    let mut at = start;
    while at < text.len() {
        let Some(c) = text[at..].chars().next() else {
            break;
        };
        match c {
            '\n' | '\r' => return at,
            '#' if depth == 0 => return at,
            ':' if colon && depth == 0 => return at,
            '\'' | '"' => {
                at = string_end(text, at).unwrap_or(text.len());
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        at += c.len_utf8();
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(name: &str, argument: &str, line: usize, short: bool) -> Token {
        Token::Directive {
            name: name.to_owned(),
            argument: argument.to_owned(),
            line,
            short,
        }
    }

    fn text(text: &str) -> Token {
        Token::Text(text.to_owned())
    }

    fn placeholder(expression: &str, line: usize) -> Token {
        Token::Placeholder {
            expression: expression.to_owned(),
            line,
        }
    }

    #[test]
    fn directive_lines_vanish() {
        let found = tokens("a\n  #if $x:\n  b\n  #end if\nc").expect("tokens");
        let expected = [
            text("a\n"),
            directive("if", "$x", 2, false),
            text("  b\n"),
            directive("end", "if", 4, false),
            text("c"),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn closed_directive_inline() {
        let found = tokens("a #if $x# b #end if# c\n").expect("tokens");
        let expected = [
            text("a "),
            directive("if", "$x", 1, false),
            text(" b "),
            directive("end", "if", 1, false),
            text(" c\n"),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn comments_and_escapes() {
        let found =
            tokens("a ## note\n## whole\n\\$x \\#if $y.z(1)[0] ${a.b}\n#* multi\nline *#\nd")
                .expect("tokens");
        let expected = [
            text("a \n$x #if "),
            placeholder("$y.z(1)[0]", 3),
            text(" "),
            placeholder("$a.b", 3),
            text("\nd"),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn short_form_closes() {
        let found = tokens("#if $x: yes\nnext").expect("tokens");
        let expected = [
            directive("if", "$x", 1, true),
            text("yes\n"),
            directive("end", "if", 2, true),
            text("next"),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn shell_text_stays() {
        let shell = "#!/bin/sh\n# comment\n#end-of-x \\${GALAXY_SLOTS:-1} $1\n";
        let found = tokens(shell).expect("tokens");
        assert_eq!(
            found,
            [text(
                "#!/bin/sh\n# comment\n#end-of-x ${GALAXY_SLOTS:-1} $1\n"
            )]
        );
        let raw = tokens("#raw\n$a #if\n#end raw\nz").expect("tokens");
        let expected = [
            directive("raw", "", 1, false),
            text("$a #if\n"),
            directive("end", "raw", 3, false),
            text("z"),
        ];
        assert_eq!(raw, expected);
    }
}
