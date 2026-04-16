//! Parses literal values: strings, interpolated strings, lists and maps.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_keyword};
use crate::Span;
use crate::ast::{Access, Expression, ExpressionKind, GStringPart, MapEntry, MapKey, Name};
use crate::token::{Token, TokenKind};

impl Parser<'_> {
    /// Decodes a `StringLiteral` token: quotes removed and escapes replaced.
    pub(super) fn string_value(&mut self, token: Token) -> String {
        let text = self.text(token);
        let quote = if text.starts_with("'''") || text.starts_with("\"\"\"") {
            3
        } else {
            1
        };
        let end = text.len().saturating_sub(quote).max(quote.min(text.len()));
        let content = &text[quote.min(text.len())..end];
        let start = token.span.start as usize + quote;
        if text.starts_with('/') {
            decode_slashy(content)
        } else {
            self.decode(content, start, text.starts_with('"'))
        }
    }

    /// Decodes the text between the parts of a GString, which has no quotes of its own.
    pub(super) fn gstring_text(&mut self, token: Token) -> String {
        let text = self.text(token);
        self.decode(text, token.span.start as usize, true)
    }

    /// A string literal used as a name, like an include source or a config selector target.
    pub(super) fn string_name(&mut self) -> Parse<Name> {
        let token = self.expect(TokenKind::StringLiteral, "a string")?;
        Ok(Name {
            text: self.string_value(token),
            span: token.span,
        })
    }

    /// Replaces the escapes of quoted strings; `start` is the byte offset of `text` in the source.
    /// In a double-quoted string, the official lexer drops an invalid escape like `\|` without
    /// an error, as its error recovery skips both characters.
    fn decode(&mut self, text: &str, start: usize, double: bool) -> String {
        let text = text.replace("\r\n", "\n");
        let mut decoded = String::with_capacity(text.len());
        let mut chars = text.char_indices().peekable();
        while let Some((index, c)) = chars.next() {
            if c != '\\' {
                decoded.push(c);
                continue;
            }
            let Some((_, next)) = chars.next() else {
                decoded.push('\\');
                break;
            };
            match next {
                'b' => decoded.push('\u{8}'),
                't' => decoded.push('\t'),
                'n' => decoded.push('\n'),
                'f' => decoded.push('\u{c}'),
                'r' => decoded.push('\r'),
                's' => decoded.push(' '),
                '"' | '\'' | '\\' | '$' => decoded.push(next),
                '\n' => {}
                'u' => match unicode_escape(&text[index + 2..]) {
                    Some(c) => {
                        decoded.push(c);
                        for _ in 0..4 {
                            chars.next();
                        }
                    }
                    None if double => {}
                    None => self.invalid_escape(next, start + index, &mut decoded),
                },
                '0'..='7' => {
                    let mut value = next.to_digit(8).unwrap_or(0);
                    let limit = if next <= '3' { 2 } else { 1 };
                    for _ in 0..limit {
                        match chars.peek().and_then(|(_, c)| c.to_digit(8)) {
                            Some(digit) => {
                                value = value * 8 + digit;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    decoded.extend(char::from_u32(value));
                }
                _ if double => {}
                _ => self.invalid_escape(next, start + index, &mut decoded),
            }
        }
        decoded
    }

    fn invalid_escape(&mut self, escaped: char, offset: usize, decoded: &mut String) {
        let span = Span::new(offset, offset + 1 + escaped.len_utf8());
        self.error_at(format!("Invalid escape sequence: '\\{escaped}'"), span);
        decoded.push(escaped);
    }
}

impl Parser<'_> {
    pub(super) fn gstring(&mut self) -> Parse<ExpressionKind> {
        let begin = self.bump();
        let slashy = self.text(begin).starts_with('/');
        if slashy {
            let message = "Slashy strings with interpolations are not supported";
            self.legacy
                .push(crate::Diagnostic::new(message, begin.span));
        }
        let mut parts: Vec<GStringPart> = Vec::new();
        loop {
            let token = self.current();
            match token.kind {
                TokenKind::GStringEnd => {
                    self.bump();
                    return Ok(ExpressionKind::GString(parts));
                }
                TokenKind::GStringText => {
                    self.bump();
                    let text = match slashy {
                        true => decode_slashy(self.text(token)),
                        false => self.gstring_text(token),
                    };
                    match parts.last_mut() {
                        Some(GStringPart::Text(previous)) => previous.push_str(&text),
                        _ => parts.push(GStringPart::Text(text)),
                    }
                }
                TokenKind::GStringPath => {
                    self.bump();
                    parts.push(GStringPart::Value(self.gstring_path(token.span)));
                }
                TokenKind::InterpolationStart => {
                    self.bump();
                    let value = self.in_braces(Self::expression)?;
                    self.expect(TokenKind::RightBrace, "'}' to close the interpolation")?;
                    parts.push(GStringPart::Value(value));
                }
                _ => return Err(self.error_here("Expected the end of the string")),
            }
        }
    }

    /// `$a.b.c` means the property `c` of the property `b` of the variable `a`.
    fn gstring_path(&self, span: Span) -> Expression {
        let text = &span.text(self.source)[1..];
        let mut offset = span.start as usize + 1;
        let mut expression: Option<Expression> = None;
        for part in text.split('.') {
            let part_span = Span::new(offset, offset + part.len());
            offset += part.len() + 1;
            expression = Some(match expression {
                None => Expression {
                    kind: ExpressionKind::Variable(part.to_owned()),
                    span: part_span,
                },
                Some(target) => Expression {
                    span: target.span.to(part_span),
                    kind: ExpressionKind::Property {
                        target: Box::new(target),
                        name: Name {
                            text: part.to_owned(),
                            span: part_span,
                        },
                        access: Access::Normal,
                    },
                },
            });
        }
        expression.unwrap_or(Expression {
            kind: ExpressionKind::Variable(String::new()),
            span,
        })
    }

    /// `[a, b]`, `[key: value]` or the empty map `[:]`.
    pub(super) fn list_or_map(&mut self) -> Parse<ExpressionKind> {
        self.expect(TokenKind::LeftBracket, "'['")?;
        if self.at(TokenKind::Colon) && self.nth(1) == TokenKind::RightBracket {
            self.bump();
            self.bump();
            return Ok(ExpressionKind::Map(Vec::new()));
        }
        self.skip_newlines();
        let kind = self.in_brackets(|parser| {
            Ok(if parser.at(TokenKind::RightBracket) {
                ExpressionKind::List(Vec::new())
            } else if parser.at_map_entry() {
                ExpressionKind::Map(parser.map_entries()?)
            } else {
                ExpressionKind::List(parser.expression_list()?)
            })
        })?;
        if self.at(TokenKind::Comma) {
            self.bump();
        }
        self.skip_newlines();
        self.expect(TokenKind::RightBracket, "']'")?;
        Ok(kind)
    }

    fn at_map_entry(&mut self) -> bool {
        if (self.at_name() || is_keyword(self.peek())) && self.nth(1) == TokenKind::Colon {
            return true;
        }
        self.lookahead(|parser| {
            parser.primary()?;
            if parser.at(TokenKind::Colon) {
                Ok(())
            } else {
                Err(Failed)
            }
        })
    }

    fn map_entries(&mut self) -> Parse<Vec<MapEntry>> {
        let mut entries = Vec::new();
        loop {
            let key =
                if (self.at_name() || is_keyword(self.peek())) && self.nth(1) == TokenKind::Colon {
                    let token = self.bump();
                    MapKey::Name(Name {
                        text: self.text(token).to_owned(),
                        span: token.span,
                    })
                } else {
                    MapKey::Expression(self.primary()?)
                };
            self.expect(TokenKind::Colon, "':' after the map key")?;
            let value = self.expression()?;
            entries.push(MapEntry { key, value });
            if !self.next_list_item() {
                return Ok(entries);
            }
        }
    }
}

/// Slashy strings only replace `\/`, line escapes and unicode escapes, as in Groovy.
fn decode_slashy(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(index) = rest.find('\\') {
        decoded.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        if let Some(tail) = after.strip_prefix('/') {
            decoded.push('/');
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('\n') {
            rest = tail;
        } else if let Some(c) = after.strip_prefix('u').and_then(unicode_escape) {
            decoded.push(c);
            rest = &after[5..];
        } else {
            decoded.push('\\');
            rest = after;
        }
    }
    decoded.push_str(rest);
    decoded
}

fn unicode_escape(text: &str) -> Option<char> {
    let digits = text.get(..4)?;
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    char::from_u32(u32::from_str_radix(digits, 16).ok()?)
}
