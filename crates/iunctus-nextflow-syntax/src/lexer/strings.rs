//! String literals and interpolated strings (GStrings).

use super::{Lexer, Mode, is_identifier_start};
use crate::token::TokenKind;

/// How many quotes of a run of `n >= 3` quotes belong to the content
/// before the closing triple quote, as in the ANTLR grammar.
fn content_quotes(n: usize) -> usize {
    if n <= 5 { n - 3 } else { 0 }
}

impl Lexer<'_> {
    pub(super) fn single_quoted(&mut self) {
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
    pub(super) fn double_quoted(&mut self) {
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

    pub(super) fn gstring_token(&mut self, triple: bool) {
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
    pub(super) fn regex_allowed(&self) -> bool {
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
    pub(super) fn slashy(&mut self) -> bool {
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
