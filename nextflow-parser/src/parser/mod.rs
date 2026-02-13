//! Builds syntax trees from tokens; holds the token cursor and error handling shared by all rules.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod call;
mod config;
mod declaration;
mod expression;
mod literal;
mod statement;
mod types;

use crate::ast::{Config, Name, Script};
use crate::lexer::tokenize;
use crate::token::{Dialect, Token, TokenKind};
use crate::{Diagnostic, Span};

/// A syntax tree with the errors found while building it.
pub struct Parsed<T> {
    pub tree: T,
    pub errors: Vec<Diagnostic>,
}

pub fn parse_script(source: &str) -> Parsed<Script> {
    let mut parser = Parser::new(source, Dialect::Script);
    let tree = parser.script();
    Parsed {
        tree,
        errors: parser.errors,
    }
}

pub fn parse_config(source: &str) -> Parsed<Config> {
    let mut parser = Parser::new(source, Dialect::Config);
    let tree = parser.config();
    Parsed {
        tree,
        errors: parser.errors,
    }
}

/// Marks a rule that already reported its error and produced no tree.
struct Failed;

type Parse<T> = Result<T, Failed>;

/// Newlines are real tokens, as in the ANTLR grammar: each rule decides where they may appear.
struct Parser<'a> {
    source: &'a str,
    dialect: Dialect,
    tokens: Vec<Token>,
    position: usize,
    errors: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, dialect: Dialect) -> Self {
        let lexed = tokenize(source, dialect);
        Self {
            source,
            dialect,
            tokens: lexed.tokens,
            position: 0,
            errors: lexed.errors,
        }
    }

    fn peek(&self) -> TokenKind {
        self.nth(0)
    }

    /// The kind of the token `n` places ahead, without skipping newlines.
    fn nth(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.position + n)
            .map_or(TokenKind::EndOfFile, |token| token.kind)
    }

    fn current(&self) -> Token {
        self.tokens[self.position.min(self.tokens.len() - 1)]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek() == kind
    }

    fn bump(&mut self) -> Token {
        let token = self.current();
        if token.kind != TokenKind::EndOfFile {
            self.position += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        let found = self.at(kind);
        if found {
            self.bump();
        }
        found
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Parse<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            Err(self.error_here(format!("Expected {what}")))
        }
    }

    fn text(&self, token: Token) -> &'a str {
        token.span.text(self.source)
    }

    /// The span from `start` to the end of the last consumed token.
    fn span_from(&self, start: Span) -> Span {
        let end = match self.position.checked_sub(1) {
            Some(last) => self.tokens[last].span.end,
            None => start.end,
        };
        Span {
            start: start.start,
            end: end.max(start.start),
        }
    }

    /// Skips newlines where the grammar allows them (`nls`).
    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    /// The kind of the first token that is not a newline.
    fn peek_past_newlines(&self) -> TokenKind {
        let mut n = 0;
        while self.nth(n) == TokenKind::Newline {
            n += 1;
        }
        self.nth(n)
    }

    fn newline_count(&self) -> usize {
        let mut count = 0;
        while self.nth(count) == TokenKind::Newline {
            count += 1;
        }
        count
    }

    fn at_separator(&self) -> bool {
        matches!(self.peek(), TokenKind::Newline | TokenKind::Semicolon)
    }

    /// Skips a statement separator (`sep`) and reports whether there was one.
    fn skip_separators(&mut self) -> bool {
        let mut found = false;
        while self.at_separator() {
            self.bump();
            found = true;
        }
        found
    }

    /// A section label such as `input:` or `main:`.
    fn at_label(&self, kind: TokenKind) -> bool {
        self.peek() == kind && self.nth(1) == TokenKind::Colon
    }

    fn error_here(&mut self, message: impl Into<String>) -> Failed {
        let span = self.current().span;
        self.error_at(message, span)
    }

    fn error_at(&mut self, message: impl Into<String>, span: Span) -> Failed {
        self.errors.push(Diagnostic::new(message, span));
        Failed
    }

    /// Tries a rule and, if it fails, rewinds as if it had never run, dropping its errors.
    fn speculate<T>(&mut self, rule: impl FnOnce(&mut Self) -> Parse<T>) -> Option<T> {
        let (position, errors) = (self.position, self.errors.len());
        match rule(self) {
            Ok(tree) => Some(tree),
            Err(Failed) => {
                self.position = position;
                self.errors.truncate(errors);
                None
            }
        }
    }

    /// Checks whether a rule would succeed here, then rewinds either way.
    fn lookahead<T>(&mut self, rule: impl FnOnce(&mut Self) -> Parse<T>) -> bool {
        let (position, errors) = (self.position, self.errors.len());
        let matched = rule(self).is_ok();
        self.position = position;
        self.errors.truncate(errors);
        matched
    }

    /// Skips to the next separator outside brackets, or to a closing bracket that ends the
    /// enclosing construct, so parsing can continue after an error.
    fn recover(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::EndOfFile => return,
                TokenKind::Newline | TokenKind::Semicolon if depth == 0 => return,
                TokenKind::LeftParen
                | TokenKind::LeftBracket
                | TokenKind::LeftBrace
                | TokenKind::InterpolationStart
                | TokenKind::GStringBegin { .. } => depth += 1,
                TokenKind::RightParen
                | TokenKind::RightBracket
                | TokenKind::RightBrace
                | TokenKind::GStringEnd => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn at_name(&self) -> bool {
        is_name(self.peek())
    }

    /// The `identifier` rule: plain identifiers and the keywords that may be used as names.
    /// A name that declares or reads a variable, which may not be `_` or a Groovy keyword.
    fn variable_name(&mut self) -> Parse<Name> {
        let name = self.name()?;
        self.check_name(&name.text, name.span);
        Ok(name)
    }

    /// Reports the names the official script parser reserves, with its messages; the config
    /// parser does not check names.
    fn check_name(&mut self, text: &str, span: Span) {
        if self.dialect == Dialect::Config {
            return;
        }
        let message = match text {
            "_" => {
                "`_` is not allowed as an identifier because it is reserved for future use".into()
            }
            "for" | "while" => format!("`{text}` loops are no longer supported"),
            "switch" => "`switch` statements are no longer supported".into(),
            _ if GROOVY_KEYWORDS.contains(&text) => {
                format!("`{text}` is not allowed as an identifier because it is a Groovy keyword")
            }
            _ => return,
        };
        self.error_at(message, span);
    }

    fn name(&mut self) -> Parse<Name> {
        if !self.at_name() {
            return Err(self.error_here("Expected a name"));
        }
        let token = self.bump();
        Ok(Name {
            text: self.text(token).to_owned(),
            span: token.span,
        })
    }
}

/// Groovy keywords that strict syntax does not use but still reserves.
const GROOVY_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "break",
    "case",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "extends",
    "final",
    "finally",
    "for",
    "goto",
    "implements",
    "import",
    "interface",
    "native",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "super",
    "switch",
    "synchronized",
    "this",
    "throws",
    "transient",
    "void",
    "volatile",
    "while",
];

fn is_name(kind: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        kind,
        Identifier
            | CapitalizedIdentifier
            | In
            | Nextflow
            | Params
            | From
            | Record
            | Agent
            | Prompt
            | Process
            | Exec
            | Input
            | Output
            | Script
            | Shell
            | Stage
            | Stub
            | Topic
            | Tuple
            | When
            | Workflow
            | Emit
            | Main
            | OnComplete
            | OnError
            | Publish
            | Take
            | IncludeConfig
    )
}

/// The `keywords` rule: words that may name a property or a map key.
fn is_keyword(kind: TokenKind) -> bool {
    use TokenKind::*;
    is_name(kind)
        || matches!(
            kind,
            As | Def
                | Import
                | Instanceof
                | Return
                | Include
                | NullLiteral
                | BooleanLiteral
                | PrimitiveType
        )
}
