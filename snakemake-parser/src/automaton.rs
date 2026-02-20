//! Finds the Snakemake statements in the tokens, following the keyword state machine of parser.py.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::Arguments;
use ruff_text_size::{TextRange, TextSize};

use crate::Error;
use crate::ast::{Directive, Statement, Value};
use crate::keyword::GlobalKeyword;
use crate::python;
use crate::token::{Kind, Token, at_line_start, tokenize};

/// Reads every Snakemake statement, at any Python nesting depth, in source order.
/// Also returns the first error in the Python of their values, which counts after keyword errors.
pub(crate) fn statements(source: &str) -> Result<(Vec<Statement>, Option<Error>), Error> {
    let mut automaton = Automaton {
        source,
        tokens: tokenize(source)?,
        position: 0,
        last_end: TextSize::default(),
        previous_end: TextSize::default(),
        deferred: None,
    };
    let statements = automaton.python()?;
    Ok((statements, automaton.deferred))
}

/// Indentation and line counting of one keyword block, as in parser.py's `KeywordState`.
struct Block {
    base: i32,
    indent: i32,
    lines: i32,
    /// Whether the text that parser.py generated last was a line break.
    after_newline: bool,
    value: Option<TextRange>,
}

enum Step {
    End,
    Skip,
    Content,
}

impl Block {
    fn new(base: i32) -> Self {
        Self {
            base,
            indent: 0,
            lines: 0,
            after_newline: false,
            value: None,
        }
    }

    fn indentation(&mut self, token: Token) {
        if matches!(token.kind, Kind::Indent | Kind::Dedent) {
            self.indent = token.column - self.base;
        }
    }

    /// Decides whether `token` ends the block.
    fn step(&mut self, token: Token) -> Step {
        if self.after_newline && token.kind == Kind::Comment {
            // parser.py does not count lines that hold only a comment.
            self.lines -= 1;
        }
        if token.kind == Kind::EndOfFile || (self.lines != 0 && self.indent <= 0) {
            return Step::End;
        }
        match token.kind {
            Kind::Newline => {
                self.lines += 1;
                self.after_newline = !token.range.is_empty();
                Step::Skip
            }
            Kind::Indent | Kind::Dedent => Step::Skip,
            Kind::Comment => {
                self.after_newline = false;
                Step::Skip
            }
            _ => Step::Content,
        }
    }

    fn add(&mut self, token: Token) {
        self.after_newline = false;
        self.value = Some(match self.value {
            Some(value) => value.cover(token.range),
            None => token.range,
        });
    }
}

/// The tokens of a block and the token that ended it.
struct Read {
    value: Option<TextRange>,
    stop: Token,
}

struct Automaton<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    position: usize,
    /// End of the last token that is not trivia, and its value before the latest token.
    last_end: TextSize,
    previous_end: TextSize,
    /// parser.py compiles the Python only after its state machine has read the whole file.
    deferred: Option<Error>,
}

impl<'a> Automaton<'a> {
    /// The top-level state: Python code, in which keywords count at the start of a line.
    fn python(&mut self) -> Result<Vec<Statement>, Error> {
        let mut statements = Vec::new();
        let mut indent = 0;
        let mut first = true;
        let mut after_newline = false;
        let mut pending = None;
        loop {
            let token = match pending.take() {
                Some(token) => token,
                None => self.next(),
            };
            match token.kind {
                Kind::Indent | Kind::Dedent => {
                    indent = token.column;
                    continue;
                }
                Kind::EndOfFile => return Ok(statements),
                _ => {}
            }
            let starts =
                first || (after_newline && at_line_start(self.source, token.range.start()));
            first = false;
            if starts
                && token.kind == Kind::Name
                && let Some((statement, stop)) = self.statement(token, indent)?
            {
                statements.push(statement);
                after_newline = true;
                pending = Some(stop);
                continue;
            }
            after_newline = token.kind == Kind::Newline && !token.range.is_empty();
        }
    }

    /// Reads the statement of `keyword`, or returns `None` if the name is not a keyword.
    fn statement(
        &mut self,
        keyword: Token,
        base: i32,
    ) -> Result<Option<(Statement, Token)>, Error> {
        let text = self.text(keyword);
        if text == "subworkflow" {
            let message = "Keyword subworkflow is deprecated. Use module directive instead.";
            return Err(self.error(message, keyword));
        }
        let unsupported = matches!(
            text,
            "rule"
                | "checkpoint"
                | "use"
                | "module"
                | "storage"
                | "ruleorder"
                | "localrules"
                | "onstart"
                | "onsuccess"
                | "onerror"
                | "resource_scopes"
        );
        if unsupported {
            return Err(self.error(format!("Keyword {text} is not supported yet."), keyword));
        }
        let Some(global) = GlobalKeyword::from_text(text) else {
            return Ok(None);
        };
        let (directive, stop) = self.directive(global, keyword, base)?;
        Ok(Some((Statement::Global(directive), stop)))
    }

    /// Reads `keyword: value` and parses the value as call arguments.
    fn directive<K>(
        &mut self,
        keyword: K,
        token: Token,
        base: i32,
    ) -> Result<(Directive<K>, Token), Error> {
        let mut block = Block::new(base);
        let colon = self.colon(token, &mut block)?;
        let read = self.read(block);
        let value = self.arguments(&read, colon).map(Value::Arguments);
        let directive = Directive {
            keyword,
            keyword_range: token.range,
            // The tree is dropped when there is an error, so any value will do.
            value: self.defer(value).unwrap_or(Value::Names(Vec::new())),
            range: TextRange::new(token.range.start(), self.end_before(read.stop)),
        };
        Ok((directive, read.stop))
    }

    /// Reads the tokens of a block after its colon.
    fn read(&mut self, mut block: Block) -> Read {
        let stop = loop {
            let token = self.next_in(&mut block);
            match block.step(token) {
                Step::End => break token,
                Step::Skip => continue,
                Step::Content => block.add(token),
            }
        };
        Read {
            value: block.value,
            stop,
        }
    }

    fn arguments(&self, read: &Read, colon: Token) -> Result<Arguments, Error> {
        python::arguments(self.source, read.value, colon.range.end())
    }

    /// Keeps the first error in Python code for later, like parser.py does.
    fn defer<T>(&mut self, result: Result<T, Error>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.deferred.get_or_insert(error);
                None
            }
        }
    }

    fn colon(&mut self, keyword: Token, block: &mut Block) -> Result<Token, Error> {
        let token = self.next_in(block);
        if token.kind == Kind::Colon {
            Ok(token)
        } else {
            let message = format!("Colon expected after keyword {}.", self.text(keyword));
            Err(self.error(message, token))
        }
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.position];
        if token.kind != Kind::EndOfFile {
            self.position += 1;
        }
        self.previous_end = self.last_end;
        if !token.is_trivia() {
            self.last_end = token.range.end();
        }
        token
    }

    /// Takes the next token into `block`, as parser.py's `consume` does before each state.
    fn next_in(&mut self, block: &mut Block) -> Token {
        let token = self.next();
        block.indentation(token);
        token
    }

    /// The end of the statement that `stop` ended: its last token before `stop`.
    fn end_before(&self, stop: Token) -> TextSize {
        if stop.is_trivia() {
            self.last_end
        } else {
            self.previous_end
        }
    }

    fn text(&self, token: Token) -> &'a str {
        &self.source[token.range]
    }

    fn error(&self, message: impl Into<String>, token: Token) -> Error {
        Error::new(self.source, message, token.range)
    }
}
