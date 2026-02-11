//! Turns source text into tokens, following `ScriptLexer.g4` and
//! `ConfigLexer.g4` from the Nextflow repository.

use crate::token::{Dialect, OPERATORS, Token, TokenKind, keyword};
use crate::{Diagnostic, Span};

/// The tokens of a source text. The last token is always `Eof`.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Diagnostic>,
}

pub fn tokenize(source: &str, dialect: Dialect) -> Lexed {
    let mut lexer = Lexer {
        src: source,
        bytes: source.as_bytes(),
        pos: 0,
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
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    dialect: Dialect,
    /// Brackets and `${` push `Default`; a string with `$` pushes `GString`.
    modes: Vec<Mode>,
    last: Option<TokenKind>,
    tokens: Vec<Token>,
    errors: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.pos < self.bytes.len() {
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
        self.push(TokenKind::Eof, end, end);
    }

    fn peek(&self, n: usize) -> u8 {
        self.byte(self.pos + n)
    }

    fn byte(&self, i: usize) -> u8 {
        self.bytes.get(i).copied().unwrap_or(0)
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.src.get(i..)?.chars().next()
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
        let start = self.pos;
        match self.peek(0) {
            b' ' | b'\t' => {
                while matches!(self.peek(0), b' ' | b'\t') {
                    self.pos += 1;
                }
            }
            b'\\' if matches!(self.peek(1), b'\n' | b'\r') => {
                // a line escape joins two lines
                self.pos += if self.peek(1) == b'\r' && self.peek(2) == b'\n' {
                    3
                } else {
                    2
                };
            }
            b'\n' => {
                self.pos += 1;
                self.push(TokenKind::Nl, start, self.pos);
            }
            b'\r' => {
                self.pos += if self.peek(1) == b'\n' { 2 } else { 1 };
                self.push(TokenKind::Nl, start, self.pos);
            }
            b'/' if self.peek(1) == b'/' => {
                self.skip_to_line_end();
                self.push(TokenKind::Nl, start, self.pos);
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

    fn skip_to_line_end(&mut self) {
        while !matches!(self.peek(0), b'\n' | b'\r') && self.pos < self.bytes.len() {
            self.pos += 1;
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        match self.src[start + 2..].find("*/") {
            Some(i) => self.pos = start + 2 + i + 2,
            None => {
                self.pos = self.bytes.len();
                self.error("Unterminated comment", start, self.pos);
            }
        }
        self.push(TokenKind::Nl, start, self.pos);
    }

    fn shebang(&mut self) {
        let start = self.pos;
        if !self.tokens.is_empty() {
            self.error(
                "Shebang comment should appear at the first line",
                start,
                start + 2,
            );
        }
        loop {
            self.skip_to_line_end();
            let next = self.pos
                + if self.peek(0) == b'\r' && self.peek(1) == b'\n' {
                    2
                } else {
                    1
                };
            if self.byte(next) == b'#' && self.byte(next + 1) == b'!' {
                self.pos = next;
            } else {
                break;
            }
        }
        self.push(TokenKind::Nl, start, self.pos);
    }

    fn number(&mut self) {
        let start = self.pos;
        let mut float = false;
        if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X') {
            self.pos += 2;
            self.digits(|b| b.is_ascii_hexdigit());
            if self.peek(0) == b'.' && self.peek(1).is_ascii_hexdigit() {
                self.pos += 1;
                self.digits(|b| b.is_ascii_hexdigit());
            }
            if matches!(self.peek(0), b'p' | b'P') {
                self.pos += 1;
                self.exponent_digits();
                float = true;
            }
        } else if self.peek(0) == b'0' && matches!(self.peek(1), b'b' | b'B') {
            self.pos += 2;
            self.digits(|b| b == b'0' || b == b'1');
        } else {
            self.digits(|b| b.is_ascii_digit());
            if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
                self.pos += 1;
                self.digits(|b| b.is_ascii_digit());
                float = true;
            }
            let signed = matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit();
            if matches!(self.peek(0), b'e' | b'E') && (self.peek(1).is_ascii_digit() || signed) {
                self.pos += 1;
                self.exponent_digits();
                float = true;
            }
        }
        match self.peek(0) {
            b'f' | b'F' | b'd' | b'D' => {
                self.pos += 1;
                float = true;
            }
            b'g' | b'G' => self.pos += 1,
            b'l' | b'L' | b'i' | b'I' if !float => self.pos += 1,
            _ => {}
        }
        let kind = if float {
            TokenKind::FloatingPointLiteral
        } else {
            TokenKind::IntegerLiteral
        };
        self.push(kind, start, self.pos);
    }

    fn digits(&mut self, digit: impl Fn(u8) -> bool) {
        while digit(self.peek(0)) || self.peek(0) == b'_' {
            self.pos += 1;
        }
    }

    fn exponent_digits(&mut self) {
        if matches!(self.peek(0), b'+' | b'-') {
            self.pos += 1;
        }
        self.digits(|b| b.is_ascii_digit());
    }

    fn identifier(&mut self) {
        let start = self.pos;
        self.pos = self.identifier_end(start, true);
        let word = &self.src[start..self.pos];
        let kind = keyword(word, self.dialect).unwrap_or_else(|| {
            if word.chars().next().is_some_and(char::is_uppercase) {
                TokenKind::CapitalizedIdentifier
            } else {
                TokenKind::Identifier
            }
        });
        self.push(kind, start, self.pos);
    }

    /// The end of the identifier that starts at `start`. Identifiers in
    /// strings may not contain `$`.
    fn identifier_end(&self, start: usize, allow_dollar: bool) -> usize {
        let mut end = start;
        for c in self.src[start..].chars() {
            let part = is_identifier_part(c) && (allow_dollar || c != '$');
            if !part || (end == start && c.is_ascii_digit()) {
                break;
            }
            end += c.len_utf8();
        }
        end
    }

    fn operator(&mut self) {
        let start = self.pos;
        let rest = &self.src[start..];
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
        self.pos += len;
        match kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBrack => {
                self.modes.push(Mode::Default)
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBrack if self.modes.len() > 1 => {
                self.modes.pop();
            }
            _ => {}
        }
        self.push(kind, start, self.pos);
    }
}

impl Lexer<'_> {
    fn single_quoted(&mut self) {
        let start = self.pos;
        let len = self.bytes.len();
        if self.src[start..].starts_with("'''") {
            match self.triple_end(start + 3, b'\'') {
                Some(end) => self.pos = end,
                None => {
                    self.pos = len;
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
            self.pos = i.min(len);
        }
        self.push(TokenKind::StringLiteral, start, self.pos);
    }

    /// A string without `$` is a plain literal. Otherwise it is a GString,
    /// which is lexed part by part in its own mode.
    fn double_quoted(&mut self) {
        let start = self.pos;
        let triple = self.src[start..].starts_with("\"\"\"");
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
                self.pos = end;
                self.push(TokenKind::StringLiteral, start, end);
            }
            None => {
                self.pos = start + open;
                self.push(TokenKind::GStringBegin { triple }, start, self.pos);
                self.modes.push(Mode::GString { triple });
            }
        }
    }

    fn gstring_token(&mut self, triple: bool) {
        let start = self.pos;
        match self.peek(0) {
            b'"' if !triple => {
                self.pos += 1;
                self.end_gstring(start);
            }
            b'"' if self.quote_run(start, b'"') >= 3
                && content_quotes(self.quote_run(start, b'"')) == 0 =>
            {
                self.pos += 3;
                self.end_gstring(start);
            }
            b'$' if self.peek(1) == b'{' => {
                self.pos += 2;
                self.push(TokenKind::GStringExprStart, start, self.pos);
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
        self.push(TokenKind::GStringEnd, start, self.pos);
        self.modes.pop();
    }

    /// `$name` or `$name.property.property`.
    fn gstring_path(&mut self) {
        let start = self.pos;
        let mut end = self.identifier_end(start + 1, false);
        while self.byte(end) == b'.'
            && self
                .char_at(end + 1)
                .is_some_and(|c| c != '$' && is_identifier_start(c))
        {
            end = self.identifier_end(end + 1, false);
        }
        self.pos = end;
        self.push(TokenKind::GStringPath, start, end);
    }

    fn gstring_text(&mut self, triple: bool) {
        let start = self.pos;
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
        self.pos = i.min(len);
        self.push(TokenKind::GStringText, start, self.pos);
    }

    /// A `/` starts a slashy string only where a value is expected.
    fn regex_allowed(&self) -> bool {
        use TokenKind::*;
        !matches!(
            self.last,
            Some(
                RBrace
                    | RBrack
                    | RParen
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
        let start = self.pos;
        let mut i = start + 1;
        loop {
            match self.byte(i) {
                0 => return false,
                b'\\' if self.byte(i + 1) == b'/' => i += 2,
                b'/' if i == start + 1 => return false,
                b'/' => break,
                b'$' if self.followed_by_letter_in_gstring(i + 1) => return false,
                _ => i += 1,
            }
        }
        self.pos = i + 1;
        self.push(TokenKind::StringLiteral, start, self.pos);
        true
    }

    fn followed_by_letter_in_gstring(&self, i: usize) -> bool {
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
