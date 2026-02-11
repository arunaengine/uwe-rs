//! Turns Nextflow source text into tokens, following the official ANTLR lexer grammars.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::token::{Dialect, OPERATORS, Token, TokenKind, keyword};
use crate::{Diagnostic, Span};

/// The tokens of a source text. The last token is always `EndOfFile`.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Diagnostic>,
}

pub fn tokenize(source: &str, dialect: Dialect) -> Lexed {
    let mut lexer = Lexer {
        source,
        bytes: source.as_bytes(),
        position: 0,
        dialect,
        modes: vec![Mode::Default],
        last: None,
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        errors: lexer.errors,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    GString { triple: bool },
}

struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    position: usize,
    dialect: Dialect,
    /// Brackets and `${` push `Default`; a string with `$` pushes `GString`.
    modes: Vec<Mode>,
    last: Option<TokenKind>,
    tokens: Vec<Token>,
    errors: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.position < self.bytes.len() {
            match *self.modes.last().unwrap() {
                Mode::Default => self.default_token(),
                Mode::GString { triple } => self.gstring_token(triple),
            }
        }
        if self.modes.iter().any(|m| matches!(m, Mode::GString { .. })) {
            let end = self.bytes.len();
            self.error("Unterminated string", end, end);
            self.push(TokenKind::GStringEnd, end, end);
        }
        let end = self.bytes.len();
        self.push(TokenKind::EndOfFile, end, end);
    }

    fn peek(&self, n: usize) -> u8 {
        self.byte(self.position + n)
    }

    fn byte(&self, i: usize) -> u8 {
        self.bytes.get(i).copied().unwrap_or(0)
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.source.get(i..)?.chars().next()
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start, end),
        });
        self.last = Some(kind);
    }

    fn error(&mut self, message: impl Into<String>, start: usize, end: usize) {
        self.errors
            .push(Diagnostic::new(message, Span::new(start, end)));
    }

    fn default_token(&mut self) {
        let start = self.position;
        match self.peek(0) {
            b' ' | b'\t' => {
                while matches!(self.peek(0), b' ' | b'\t') {
                    self.position += 1;
                }
            }
            b'\\' if matches!(self.peek(1), b'\n' | b'\r') => {
                // a line escape joins two lines
                self.position += if self.peek(1) == b'\r' && self.peek(2) == b'\n' {
                    3
                } else {
                    2
                };
            }
            b'\n' => {
                self.position += 1;
                self.push(TokenKind::Newline, start, self.position);
            }
            b'\r' => {
                self.position += if self.peek(1) == b'\n' { 2 } else { 1 };
                self.push(TokenKind::Newline, start, self.position);
            }
            b'/' if self.peek(1) == b'/' => {
                self.skip_to_newline();
                self.push(TokenKind::Newline, start, self.position);
            }
            b'/' if self.peek(1) == b'*' => self.block_comment(),
            b'#' if self.peek(1) == b'!' && self.dialect == Dialect::Script => self.shebang(),
            b'"' => self.double_quoted(),
            b'\'' => self.single_quoted(),
            b'/' if self.regex_allowed() && self.slashy() => {}
            b'0'..=b'9' => self.number(),
            b'.' if self.peek(1).is_ascii_digit() => self.number(),
            _ => match self.char_at(start) {
                Some(c) if is_identifier_start(c) => self.identifier(),
                _ => self.operator(),
            },
        }
    }

    fn skip_to_newline(&mut self) {
        while !matches!(self.peek(0), b'\n' | b'\r') && self.position < self.bytes.len() {
            self.position += 1;
        }
    }

    fn block_comment(&mut self) {
        let start = self.position;
        match self.source[start + 2..].find("*/") {
            Some(i) => self.position = start + 2 + i + 2,
            None => {
                self.position = self.bytes.len();
                self.error("Unterminated comment", start, self.position);
            }
        }
        self.push(TokenKind::Newline, start, self.position);
    }

    fn shebang(&mut self) {
        let start = self.position;
        if !self.tokens.is_empty() {
            self.error(
                "Shebang comment should appear at the first line",
                start,
                start + 2,
            );
        }
        loop {
            self.skip_to_newline();
            let next = self.position
                + if self.peek(0) == b'\r' && self.peek(1) == b'\n' {
                    2
                } else {
                    1
                };
            if self.byte(next) == b'#' && self.byte(next + 1) == b'!' {
                self.position = next;
            } else {
                break;
            }
        }
        self.push(TokenKind::Newline, start, self.position);
    }

    fn number(&mut self) {
        let start = self.position;
        let mut float = false;
        if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X') {
            self.position += 2;
            self.digits(|b| b.is_ascii_hexdigit());
            if self.peek(0) == b'.' && self.peek(1).is_ascii_hexdigit() {
                self.position += 1;
                self.digits(|b| b.is_ascii_hexdigit());
            }
            if matches!(self.peek(0), b'p' | b'P') {
                self.position += 1;
                self.exponent_digits();
                float = true;
            }
        } else if self.peek(0) == b'0' && matches!(self.peek(1), b'b' | b'B') {
            self.position += 2;
            self.digits(|b| b == b'0' || b == b'1');
        } else {
            self.digits(|b| b.is_ascii_digit());
            if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
                self.position += 1;
                self.digits(|b| b.is_ascii_digit());
                float = true;
            }
            let signed = matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit();
            if matches!(self.peek(0), b'e' | b'E') && (self.peek(1).is_ascii_digit() || signed) {
                self.position += 1;
                self.exponent_digits();
                float = true;
            }
        }
        match self.peek(0) {
            b'f' | b'F' | b'd' | b'D' => {
                self.position += 1;
                float = true;
            }
            b'g' | b'G' => self.position += 1,
            b'l' | b'L' | b'i' | b'I' if !float => self.position += 1,
            _ => {}
        }
        let kind = if float {
            TokenKind::FloatingPointLiteral
        } else {
            TokenKind::IntegerLiteral
        };
        self.push(kind, start, self.position);
    }

    fn digits(&mut self, digit: impl Fn(u8) -> bool) {
        while digit(self.peek(0)) || self.peek(0) == b'_' {
            self.position += 1;
        }
    }

    fn exponent_digits(&mut self) {
        if matches!(self.peek(0), b'+' | b'-') {
            self.position += 1;
        }
        self.digits(|b| b.is_ascii_digit());
    }

    fn identifier(&mut self) {
        let start = self.position;
        self.position = self.identifier_end(start, true);
        let word = &self.source[start..self.position];
        let kind = keyword(word, self.dialect).unwrap_or_else(|| {
            if word.chars().next().is_some_and(char::is_uppercase) {
                TokenKind::CapitalizedIdentifier
            } else {
                TokenKind::Identifier
            }
        });
        self.push(kind, start, self.position);
    }

    /// The end of the identifier that starts at `start`. Identifiers in
    /// strings may not contain `$`.
    fn identifier_end(&self, start: usize, allow_dollar: bool) -> usize {
        let mut end = start;
        for c in self.source[start..].chars() {
            let part = is_identifier_part(c) && (allow_dollar || c != '$');
            if !part || (end == start && c.is_ascii_digit()) {
                break;
            }
            end += c.len_utf8();
        }
        end
    }

    fn operator(&mut self) {
        let start = self.position;
        let rest = &self.source[start..];
        let not_in = rest.starts_with("!in")
            && matches!(
                self.peek(3),
                b' ' | b'\t' | b'\r' | b'\n' | b'[' | b'(' | b'{'
            );
        let not_instanceof = rest.starts_with("!instanceof")
            && matches!(self.peek(11), b' ' | b'\t' | b'\r' | b'\n');
        let (kind, len) = if not_instanceof {
            (TokenKind::NotInstanceof, 11)
        } else if not_in {
            (TokenKind::NotIn, 3)
        } else if let Some((text, kind)) = OPERATORS.iter().find(|(t, _)| rest.starts_with(t)) {
            (*kind, text.len())
        } else {
            let len = self.char_at(start).map_or(1, char::len_utf8);
            self.error(
                format!("Unexpected character: '{}'", &rest[..len]),
                start,
                start + len,
            );
            (TokenKind::Unexpected, len)
        };
        self.position += len;
        match kind {
            TokenKind::LeftParen | TokenKind::LeftBrace | TokenKind::LeftBracket => {
                self.modes.push(Mode::Default)
            }
            TokenKind::RightParen | TokenKind::RightBrace | TokenKind::RightBracket
                if self.modes.len() > 1 =>
            {
                self.modes.pop();
            }
            _ => {}
        }
        self.push(kind, start, self.position);
    }
}

impl Lexer<'_> {
    fn single_quoted(&mut self) {
        let start = self.position;
        let len = self.bytes.len();
        if self.source[start..].starts_with("'''") {
            match self.triple_end(start + 3, b'\'') {
                Some(end) => self.position = end,
                None => {
                    self.position = len;
                    self.error("Unterminated string", start, len);
                }
            }
        } else {
            let mut i = start + 1;
            loop {
                if i >= len {
                    self.error("Unterminated string", start, len);
                    break;
                }
                match self.byte(i) {
                    b'\\' => i = self.skip_escape(i),
                    b'\'' => {
                        i += 1;
                        break;
                    }
                    b'\n' | b'\r' => {
                        self.error("Unterminated string", start, i);
                        break;
                    }
                    _ => i += 1,
                }
            }
            self.position = i.min(len);
        }
        self.push(TokenKind::StringLiteral, start, self.position);
    }

    /// A string without `$` is a plain literal. Otherwise it is a GString,
    /// which is lexed part by part in its own mode.
    fn double_quoted(&mut self) {
        let start = self.position;
        let triple = self.source[start..].starts_with("\"\"\"");
        let open = if triple { 3 } else { 1 };
        let mut i = start + open;
        let literal_end = loop {
            if i >= self.bytes.len() {
                break None;
            }
            match self.byte(i) {
                b'\\' => i = self.skip_escape(i),
                b'$' => break None,
                b'"' if triple => {
                    let n = self.quote_run(i, b'"');
                    if n >= 3 {
                        break Some(i + content_quotes(n) + 3);
                    }
                    i += n;
                }
                b'"' => break Some(i + 1),
                b'\n' | b'\r' if !triple => break None,
                _ => i += 1,
            }
        };
        match literal_end {
            Some(end) => {
                self.position = end;
                self.push(TokenKind::StringLiteral, start, end);
            }
            None => {
                self.position = start + open;
                self.push(TokenKind::GStringBegin { triple }, start, self.position);
                self.modes.push(Mode::GString { triple });
            }
        }
    }

    fn gstring_token(&mut self, triple: bool) {
        let start = self.position;
        match self.peek(0) {
            b'"' if !triple => {
                self.position += 1;
                self.end_gstring(start);
            }
            b'"' if self.quote_run(start, b'"') >= 3
                && content_quotes(self.quote_run(start, b'"')) == 0 =>
            {
                self.position += 3;
                self.end_gstring(start);
            }
            b'$' if self.peek(1) == b'{' => {
                self.position += 2;
                self.push(TokenKind::InterpolationStart, start, self.position);
                self.modes.push(Mode::Default);
            }
            b'$' if self
                .char_at(start + 1)
                .is_some_and(|c| c != '$' && is_identifier_start(c)) =>
            {
                self.gstring_path()
            }
            b'\n' | b'\r' if !triple => {
                self.error("Unterminated string", start, start);
                self.end_gstring(start);
            }
            _ => self.gstring_text(triple),
        }
    }

    fn end_gstring(&mut self, start: usize) {
        self.push(TokenKind::GStringEnd, start, self.position);
        self.modes.pop();
    }

    /// `$name` or `$name.property.property`.
    fn gstring_path(&mut self) {
        let start = self.position;
        let mut end = self.identifier_end(start + 1, false);
        while self.byte(end) == b'.'
            && self
                .char_at(end + 1)
                .is_some_and(|c| c != '$' && is_identifier_start(c))
        {
            end = self.identifier_end(end + 1, false);
        }
        self.position = end;
        self.push(TokenKind::GStringPath, start, end);
    }

    fn gstring_text(&mut self, triple: bool) {
        let start = self.position;
        let len = self.bytes.len();
        let mut i = start;
        while i < len {
            match self.byte(i) {
                b'\\' => i = self.skip_escape(i),
                b'$' if i == start => {
                    // a `$` that starts neither a path nor an expression
                    self.error("Unexpected character: '$'", i, i + 1);
                    i += 1;
                }
                b'$' => break,
                b'"' if !triple => break,
                b'"' => {
                    let n = self.quote_run(i, b'"');
                    if n >= 3 {
                        i += content_quotes(n);
                        break;
                    }
                    i += n;
                }
                b'\n' | b'\r' if !triple => break,
                _ => i += 1,
            }
        }
        self.position = i.min(len);
        self.push(TokenKind::GStringText, start, self.position);
    }

    /// A `/` starts a slashy string only where a value is expected.
    fn regex_allowed(&self) -> bool {
        use TokenKind::*;
        !matches!(
            self.last,
            Some(
                RightBrace
                    | RightBracket
                    | RightParen
                    | GStringEnd
                    | NullLiteral
                    | StringLiteral
                    | BooleanLiteral
                    | IntegerLiteral
                    | FloatingPointLiteral
                    | Identifier
                    | CapitalizedIdentifier
            )
        )
    }

    /// Lexes `/.../` if it is a complete slashy string, else lexes nothing.
    fn slashy(&mut self) -> bool {
        let start = self.position;
        let mut i = start + 1;
        loop {
            match self.byte(i) {
                0 => return false,
                b'\\' if self.byte(i + 1) == b'/' => i += 2,
                b'/' if i == start + 1 => return false,
                b'/' => break,
                b'$' if self.starts_interpolation(i + 1) => return false,
                _ => i += 1,
            }
        }
        self.position = i + 1;
        self.push(TokenKind::StringLiteral, start, self.position);
        true
    }

    fn starts_interpolation(&self, i: usize) -> bool {
        match self.char_at(i) {
            Some('$') | None => false,
            Some(c) => c == '{' || is_identifier_start(c),
        }
    }

    /// The end of a triple-quoted string whose content starts at `i`.
    fn triple_end(&self, mut i: usize, quote: u8) -> Option<usize> {
        while i < self.bytes.len() {
            match self.byte(i) {
                b'\\' => i = self.skip_escape(i),
                b if b == quote => {
                    let n = self.quote_run(i, quote);
                    if n >= 3 {
                        return Some(i + content_quotes(n) + 3);
                    }
                    i += n;
                }
                _ => i += 1,
            }
        }
        None
    }

    fn quote_run(&self, i: usize, quote: u8) -> usize {
        self.bytes[i..].iter().take_while(|&&b| b == quote).count()
    }

    /// Skips a backslash and the character after it. A line escape may
    /// be a backslash followed by `\r\n`.
    fn skip_escape(&self, i: usize) -> usize {
        if self.byte(i + 1) == b'\r' && self.byte(i + 2) == b'\n' {
            i + 3
        } else {
            i + 2
        }
    }
}

/// How many quotes of a run of `n >= 3` quotes belong to the content
/// before the closing triple quote, as in the ANTLR grammar.
fn content_quotes(n: usize) -> usize {
    if n <= 5 { n - 3 } else { 0 }
}

pub(crate) fn is_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphabetic())
}

pub(crate) fn is_identifier_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphanumeric())
}
