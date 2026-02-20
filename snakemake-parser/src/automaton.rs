//! Finds the Snakemake statements in the tokens, following the keyword state machine of parser.py.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Error;
use crate::ast::Statement;
use crate::keyword::GlobalKeyword;
use crate::token::{Kind, Token, at_line_start, tokenize};

/// Reads every Snakemake statement, at any Python nesting depth, in source order.
pub(crate) fn statements(source: &str) -> Result<Vec<Statement>, Error> {
    let mut automaton = Automaton {
        source,
        tokens: tokenize(source)?,
        position: 0,
    };
    automaton.python()
}

struct Automaton<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    position: usize,
}

impl<'a> Automaton<'a> {
    /// The top-level state: Python code, in which keywords count at the start of a line.
    fn python(&mut self) -> Result<Vec<Statement>, Error> {
        let mut first = true;
        let mut after_newline = false;
        loop {
            let token = self.next();
            match token.kind {
                Kind::Indent | Kind::Dedent => continue,
                Kind::EndOfFile => return Ok(Vec::new()),
                _ => {}
            }
            let starts =
                first || (after_newline && at_line_start(self.source, token.range.start()));
            first = false;
            if starts && token.kind == Kind::Name {
                self.statement(token)?;
            }
            after_newline = token.kind == Kind::Newline && !token.range.is_empty();
        }
    }

    /// Rejects the Snakemake keywords, which are not supported yet.
    fn statement(&self, keyword: Token) -> Result<(), Error> {
        let text = self.text(keyword);
        if text == "subworkflow" {
            let message = "Keyword subworkflow is deprecated. Use module directive instead.";
            return Err(self.error(message, keyword));
        }
        let statement = matches!(text, "rule" | "checkpoint" | "use" | "module" | "storage");
        if statement || GlobalKeyword::from_text(text).is_some() {
            return Err(self.error(format!("Keyword {text} is not supported yet."), keyword));
        }
        Ok(())
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.position];
        if token.kind != Kind::EndOfFile {
            self.position += 1;
        }
        token
    }

    fn text(&self, token: Token) -> &'a str {
        &self.source[token.range]
    }

    fn error(&self, message: impl Into<String>, token: Token) -> Error {
        Error::new(self.source, message, token.range)
    }
}
