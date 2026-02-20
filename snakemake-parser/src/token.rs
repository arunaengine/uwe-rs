//! Splits a Snakefile into the tokens that the keyword state machine reads, using ruff's lexer.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::token::TokenKind;
use ruff_python_parser::lexer::lex;
use ruff_python_parser::{Mode, ParseErrorType, ParseOptions, parse_unchecked};
use ruff_text_size::{TextRange, TextSize};

use crate::Error;

/// The token classes that parser.py tells apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An identifier or a Python keyword, both `NAME` in Python's tokenize module.
    Name,
    /// A string or bytes literal, but not an f-string.
    String,
    /// A whole f-string or t-string; parser.py never looks inside them.
    Template,
    Comment,
    /// A logical or non-logical line break; empty at the end of a file without one.
    Newline,
    Indent,
    Dedent,
    EndOfFile,
    Colon,
    Comma,
    Greater,
    Star,
    Equal,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) range: TextRange,
    /// For indents and dedents: the whitespace characters before the line's first token.
    pub(crate) column: i32,
}

impl Token {
    /// Newlines, indentation and comments, which never belong to a keyword's value.
    pub(crate) fn is_trivia(self) -> bool {
        matches!(
            self.kind,
            Kind::Newline | Kind::Indent | Kind::Dedent | Kind::Comment | Kind::EndOfFile
        )
    }
}

/// Lexes the whole file; the result always ends with an end-of-file token.
pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, Error> {
    let mut lexer = lex(source, Mode::Module);
    let mut tokens = Vec::new();
    let mut nesting = 0;
    let mut start = TextSize::default();
    loop {
        let kind = lexer.next_token();
        let range = lexer.current_range();
        let kind = match kind {
            TokenKind::Unknown => return Err(lexical_error(source, range)),
            TokenKind::FStringStart | TokenKind::TStringStart => {
                if nesting == 0 {
                    start = range.start();
                }
                nesting += 1;
                continue;
            }
            TokenKind::FStringEnd | TokenKind::TStringEnd => {
                nesting -= 1;
                if nesting > 0 {
                    continue;
                }
                let range = TextRange::new(start, range.end());
                tokens.push(Token {
                    kind: Kind::Template,
                    range,
                    column: 0,
                });
                continue;
            }
            TokenKind::EndOfFile if nesting > 0 => {
                let range = TextRange::new(start, range.end());
                return Err(Error::new(source, "unterminated f-string", range));
            }
            _ if nesting > 0 => continue,
            TokenKind::EndOfFile => Kind::EndOfFile,
            TokenKind::Identifier => Kind::Name,
            kind if kind.is_keyword() => Kind::Name,
            TokenKind::String => Kind::String,
            TokenKind::Comment => Kind::Comment,
            TokenKind::Newline | TokenKind::NonLogicalNewline => Kind::Newline,
            TokenKind::Indent => Kind::Indent,
            TokenKind::Dedent => Kind::Dedent,
            TokenKind::Colon => Kind::Colon,
            TokenKind::Comma => Kind::Comma,
            TokenKind::Greater => Kind::Greater,
            TokenKind::Star => Kind::Star,
            TokenKind::Equal => Kind::Equal,
            _ => Kind::Other,
        };
        // Indentation is ASCII, so its length in bytes is its length in characters.
        let column = match kind {
            Kind::Indent => line_prefix(source, range.end()).len() as i32,
            // Python's tokenize puts the dedents at the end of a file into column 0.
            Kind::Dedent if range.start().to_usize() == source.len() => 0,
            Kind::Dedent => line_prefix(source, range.start()).len() as i32,
            _ => 0,
        };
        tokens.push(Token {
            kind,
            range,
            column,
        });
        if kind == Kind::EndOfFile {
            return Ok(tokens);
        }
    }
}

/// Whether only whitespace precedes `offset` on its physical line.
pub(crate) fn at_line_start(source: &str, offset: TextSize) -> bool {
    let prefix = line_prefix(source, offset);
    prefix
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\x0c'))
}

/// The text between the start of the physical line and `offset`.
fn line_prefix(source: &str, offset: TextSize) -> &str {
    let before = &source[..offset.to_usize()];
    let line_start = before.rfind(['\n', '\r']).map_or(0, |index| index + 1);
    &before[line_start..]
}

/// The lexer only reports that a token is invalid; the parser knows the reason.
fn lexical_error(source: &str, range: TextRange) -> Error {
    let parsed = parse_unchecked(source, ParseOptions::from(Mode::Module));
    let message = parsed
        .errors()
        .iter()
        .find(|error| {
            matches!(error.error, ParseErrorType::Lexical(_))
                && error.location.start() == range.start()
        })
        .map_or_else(
            || "invalid token".to_string(),
            |error| error.error.to_string(),
        );
    Error::new(source, message, range)
}
