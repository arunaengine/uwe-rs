//! Turns Nextflow source text into tokens, following the official ANTLR lexer grammars.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::token::{Dialect, OPERATORS, Token, TokenKind, keyword};
use crate::{Diagnostic, Span};

/// The tokens of a source text. The last token is always `EndOfFile`.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Diagnostic>,
    /// Legacy syntax that strict syntax rejects but uwe accepts, like `node.@name`.
    pub legacy: Vec<Diagnostic>,
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
        legacy: Vec::new(),
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        errors: lexer.errors,
        legacy: lexer.legacy,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    GString {
        triple: bool,
    },
    /// A slashy string with interpolations, which strict syntax does not have.
    Slashy,
    /// A dollar slashy string, `$/.../$`, which strict syntax does not have.
    DollarSlashy,
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
    legacy: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.position < self.bytes.len() {
            match *self.modes.last().unwrap() {
                Mode::Default => self.default_token(),
                Mode::GString { triple } => self.gstring_token(triple),
                Mode::Slashy => self.slashy_token(),
                Mode::DollarSlashy => self.dollar_slashy_token(),
            }
        }
        if self
            .modes
            .iter()
            .any(|m| matches!(m, Mode::GString { .. } | Mode::Slashy | Mode::DollarSlashy))
        {
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
            // Legacy syntax: a dollar slashy string, which the parser reports.
            b'$' if self.peek(1) == b'/' && self.regex_allowed() => {
                self.position += 2;
                self.push(
                    TokenKind::GStringBegin { triple: true },
                    start,
                    self.position,
                );
                self.modes.push(Mode::DollarSlashy);
            }
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
        let mut radix = 10;
        // Without a digit after it, `0x` is the number `0` and then a name, as in ANTLR.
        if self.peek(0) == b'0'
            && matches!(self.peek(1), b'x' | b'X')
            && self.peek(2).is_ascii_hexdigit()
        {
            radix = 16;
            self.position += 2;
            self.digits(|b| b.is_ascii_hexdigit());
            // A hex fraction needs a binary exponent, as in `0x1.8p3`; so `0x10.c` is the
            // property `c` of `0x10`, as the longest token the official lexer can match.
            let mut exponent = 0;
            if self.peek(0) == b'.' {
                exponent = 1;
                while self.peek(exponent).is_ascii_hexdigit() || self.peek(exponent) == b'_' {
                    exponent += 1;
                }
            }
            let sign = usize::from(matches!(self.peek(exponent + 1), b'+' | b'-'));
            if matches!(self.peek(exponent), b'p' | b'P')
                && self.peek(exponent + 1 + sign).is_ascii_digit()
            {
                self.position += exponent + 1;
                self.exponent_digits();
                float = true;
            }
        } else if self.peek(0) == b'0'
            && matches!(self.peek(1), b'b' | b'B')
            && matches!(self.peek(2), b'0' | b'1')
        {
            radix = 2;
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
        let numeral = self.position;
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
        self.check_number(start, numeral, float, radix);
    }

    /// Reports numbers that the official lexer rejects, or that its AST builder cannot convert
    /// because it reads floating point numbers as decimals.
    fn check_number(&mut self, start: usize, end: usize, float: bool, radix: u32) {
        let text = &self.bytes[start..end];
        let digit = |b: Option<&u8>| b.is_some_and(|b| char::from(*b).is_digit(radix));
        // Underscores may only stand between digits.
        let misplaced = (0..text.len()).any(|i| {
            let before = text[..i].iter().rev().find(|b| **b != b'_');
            let after = text[i..].iter().find(|b| **b != b'_');
            text[i] == b'_' && !(digit(before) && digit(after))
        });
        let octal = !float && radix == 10 && text.len() > 1 && text[0] == b'0';
        let message = if misplaced {
            "Number ending with underscores is invalid".to_owned()
        } else if float && radix == 16 {
            let marker = char::from(text[1]);
            format!(
                "Character {marker} is neither a decimal digit number, decimal point, nor \"e\" notation exponential mark."
            )
        } else if octal && text.iter().any(|b| matches!(b, b'8' | b'9')) {
            "Invalid octal number".to_owned()
        } else {
            return;
        };
        self.error(message, start, self.position);
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
        } else if rest.starts_with('@') && self.last == Some(TokenKind::Dot) {
            // Legacy syntax: `node.@name` reads an attribute or a field directly.
            let span = Span::new(start, start + 1);
            self.legacy
                .push(Diagnostic::new("Unexpected character: '@'", span));
            (TokenKind::At, 1)
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

    /// The next token inside a slashy string with interpolations.
    fn slashy_token(&mut self) {
        let start = self.position;
        match self.peek(0) {
            b'/' => {
                self.position += 1;
                self.end_gstring(start);
            }
            b'$' if self.peek(1) == b'{' => {
                self.position += 2;
                self.push(TokenKind::InterpolationStart, start, self.position);
                self.modes.push(Mode::Default);
            }
            b'$' if self.starts_interpolation(start + 1) => self.gstring_path(),
            _ => {
                let mut i = start;
                while i < self.bytes.len() {
                    match self.byte(i) {
                        b'\\' if self.byte(i + 1) == b'/' => i += 2,
                        b'$' if i > start && self.starts_interpolation(i + 1) => break,
                        b'/' => break,
                        _ => i += 1,
                    }
                }
                self.position = i.max(start + 1).min(self.bytes.len());
                self.push(TokenKind::GStringText, start, self.position);
            }
        }
    }

    /// The next token inside a dollar slashy string: `/$` ends it, `$$` and `$/` stand for `$`
    /// and `/`, and a backslash is text.
    fn dollar_slashy_token(&mut self) {
        let start = self.position;
        match (self.peek(0), self.peek(1)) {
            (b'/', b'$') => {
                self.position += 2;
                self.end_gstring(start);
            }
            (b'$', b'{') => {
                self.position += 2;
                self.push(TokenKind::InterpolationStart, start, self.position);
                self.modes.push(Mode::Default);
            }
            (b'$', _) if self.starts_interpolation(start + 1) => self.gstring_path(),
            _ => {
                let mut i = start;
                while i < self.bytes.len() {
                    match (self.byte(i), self.byte(i + 1)) {
                        (b'$', b'$' | b'/') => i += 2,
                        (b'/', b'$') => break,
                        (b'$', _) if i > start && self.starts_interpolation(i + 1) => break,
                        _ => i += 1,
                    }
                }
                self.position = i.max(start + 1).min(self.bytes.len());
                self.push(TokenKind::GStringText, start, self.position);
            }
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

    /// Lexes `/.../` if it is a complete slashy string, else lexes nothing. As the official
    /// lexer takes the longest match, `\/` may also be a backslash and the closing slash.
    fn slashy(&mut self) -> bool {
        let start = self.position;
        let mut i = start + 1;
        let mut shorter = None;
        let end = loop {
            match self.byte(i) {
                0 => break shorter,
                b'\\' if self.byte(i + 1) == b'/' => {
                    shorter = Some(i + 2);
                    i += 2;
                }
                b'/' if i == start + 1 => return false,
                b'/' => break Some(i + 1),
                b'$' if self.starts_interpolation(i + 1) && shorter.is_none() => {
                    // Legacy syntax: a slashy string with interpolations, which the parser
                    // reports.
                    self.position = start + 1;
                    self.push(TokenKind::GStringBegin { triple: false }, start, start + 1);
                    self.modes.push(Mode::Slashy);
                    return true;
                }
                b'$' if self.starts_interpolation(i + 1) => break shorter,
                _ => i += 1,
            }
        };
        let Some(end) = end else {
            return false;
        };
        self.position = end;
        self.push(TokenKind::StringLiteral, start, end);
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
