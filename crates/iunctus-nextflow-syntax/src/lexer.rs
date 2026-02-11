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
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    dialect: Dialect,
    /// Brackets push `Default`.
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
            }
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

pub(crate) fn is_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphabetic())
}

pub(crate) fn is_identifier_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphanumeric())
}
