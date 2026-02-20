//! Finds the Snakemake statements in the tokens, following the keyword state machine of parser.py.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::{Arguments, Expr};
use ruff_text_size::{TextRange, TextSize};

use crate::Error;
use crate::ast::{Directive, Name, Statement, Value};
use crate::keyword::GlobalKeyword;
use crate::python::{self, Form};
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
pub(crate) struct Block {
    base: i32,
    indent: i32,
    lines: i32,
    /// Whether the text that parser.py generated last was a line break.
    pub(crate) after_newline: bool,
    content: u32,
    value: Option<TextRange>,
}

pub(crate) enum Step {
    End,
    Skip,
    Content,
}

impl Block {
    pub(crate) fn new(base: i32) -> Self {
        Self {
            base,
            indent: 0,
            lines: 0,
            after_newline: false,
            content: 0,
            value: None,
        }
    }

    fn indentation(&mut self, token: Token) {
        if matches!(token.kind, Kind::Indent | Kind::Dedent) {
            self.indent = token.column - self.base;
        }
    }

    /// Decides whether `token` ends the block; `run:` blocks only end after some content.
    pub(crate) fn step(&mut self, token: Token, needs_content: bool) -> Step {
        if self.after_newline && token.kind == Kind::Comment {
            // parser.py does not count lines that hold only a comment.
            self.lines -= 1;
        }
        let ready = !needs_content || self.content > 0;
        if token.kind == Kind::EndOfFile || (ready && self.lines != 0 && self.indent <= 0) {
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
        self.content += 1;
        self.after_newline = false;
        self.value = Some(match self.value {
            Some(value) => value.cover(token.range),
            None => token.range,
        });
    }

    /// The indentation that parser.py gives to the keywords inside this block.
    pub(crate) fn inner(&self) -> i32 {
        self.base + self.indent
    }
}

/// What a keyword block holds, which decides how its tokens are checked and parsed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Arguments(Form),
    /// Statements of a handler.
    Handler,
    /// Statements of a `run:` block, which only ends after some content.
    Run,
    /// Rule names separated by `,` or `>`.
    Names(Kind),
    /// Arguments of `resource_scopes`, whose strings must be `local` or `global`.
    Scopes,
}

/// The tokens of a block and the token that ended it.
pub(crate) struct Read {
    value: Option<TextRange>,
    names: Vec<Name>,
    pub(crate) stop: Token,
}

pub(crate) struct Automaton<'a> {
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
        let (statement, stop) = match text {
            "rule" | "checkpoint" => {
                let (rule, stop) = self.rule(keyword, base)?;
                (Statement::Rule(rule), stop)
            }
            "use" => {
                let (rule, stop) = self.use_rule(keyword, base)?;
                (Statement::UseRule(rule), stop)
            }
            "module" | "storage" => {
                return Err(self.error(format!("Keyword {text} is not supported yet."), keyword));
            }
            "subworkflow" => {
                let message = "Keyword subworkflow is deprecated. Use module directive instead.";
                return Err(self.error(message, keyword));
            }
            _ => {
                let Some(global) = GlobalKeyword::from_text(text) else {
                    return Ok(None);
                };
                let shape = match global {
                    GlobalKeyword::RuleOrder => Shape::Names(Kind::Greater),
                    GlobalKeyword::LocalRules => Shape::Names(Kind::Comma),
                    GlobalKeyword::OnStart | GlobalKeyword::OnSuccess | GlobalKeyword::OnError => {
                        Shape::Handler
                    }
                    GlobalKeyword::ResourceScopes => Shape::Scopes,
                    _ => Shape::Arguments(Form::Plain),
                };
                let (directive, stop) = self.directive(global, keyword, base, shape)?;
                (Statement::Global(directive), stop)
            }
        };
        Ok(Some((statement, stop)))
    }

    /// Reads `keyword: value` and parses the value according to `shape`.
    pub(crate) fn directive<K>(
        &mut self,
        keyword: K,
        token: Token,
        base: i32,
        shape: Shape,
    ) -> Result<(Directive<K>, Token), Error> {
        let mut block = Block::new(base);
        let colon = self.colon(token, &mut block)?;
        let read = self.read(block, shape)?;
        let value = match shape {
            Shape::Arguments(form) => self.arguments(&read, colon, form).map(Value::Arguments),
            Shape::Scopes => self
                .arguments(&read, colon, Form::Plain)
                .map(Value::Arguments),
            Shape::Handler | Shape::Run => match read.value {
                Some(range) => python::body(self.source, range).map(Value::Body),
                None => Err(self.error("Expected an indented block", read.stop)),
            },
            Shape::Names(_) => Ok(Value::Names(read.names)),
        };
        let directive = Directive {
            keyword,
            keyword_range: token.range,
            // The tree is dropped when there is an error, so any value will do.
            value: self.defer(value).unwrap_or(Value::Names(Vec::new())),
            range: TextRange::new(token.range.start(), self.end_before(read.stop)),
        };
        Ok((directive, read.stop))
    }

    /// Reads the tokens of a block after its colon, checking them as parser.py does.
    pub(crate) fn read(&mut self, mut block: Block, shape: Shape) -> Result<Read, Error> {
        let mut names: Vec<Name> = Vec::new();
        let mut separated = true;
        let mut previous = Kind::Colon;
        let mut resource = "";
        let stop = loop {
            let token = self.next_in(&mut block);
            match block.step(token, shape == Shape::Run) {
                Step::End => break token,
                Step::Skip => {
                    if matches!(token.kind, Kind::Newline | Kind::Comment) {
                        previous = token.kind;
                    }
                    continue;
                }
                Step::Content => {}
            }
            match shape {
                Shape::Names(separator) => {
                    self.name_list(token, separator, &mut names, &mut separated)?;
                }
                Shape::Scopes => {
                    if token.kind == Kind::Name {
                        resource = self.text(token);
                    } else if token.kind == Kind::String && previous == Kind::Equal {
                        let scope = self.text(token);
                        let mut inner = scope.chars();
                        inner.next();
                        inner.next_back();
                        if !matches!(inner.as_str(), "local" | "global") {
                            let message = format!(
                                "Invalid scope: {resource}={scope}. Scope must be set to either 'local' or 'global'"
                            );
                            return Err(self.error(message, token));
                        }
                    }
                }
                _ => {}
            }
            previous = token.kind;
            block.add(token);
        };
        if block.value.is_none() && matches!(shape, Shape::Arguments(Form::Shell | Form::Command)) {
            let message = "Command must be given as string after the shell keyword.";
            return Err(self.error(message, stop));
        }
        Ok(Read {
            value: block.value,
            names,
            stop,
        })
    }

    /// Checks one token of `localrules` or `ruleorder`; adjacent names concatenate like strings.
    fn name_list(
        &self,
        token: Token,
        separator: Kind,
        names: &mut Vec<Name>,
        separated: &mut bool,
    ) -> Result<(), Error> {
        if token.kind == Kind::Name {
            match names.last_mut() {
                Some(last) if !*separated => {
                    last.text.push_str(self.text(token));
                    last.range = last.range.cover(token.range);
                }
                _ => names.push(self.name(token)),
            }
            *separated = false;
            return Ok(());
        }
        if token.kind == separator && !*separated {
            *separated = true;
            return Ok(());
        }
        let message = if token.kind == separator {
            "Expected a rule name before the separator."
        } else if separator == Kind::Greater {
            "Expected a descending order of rule names, e.g. rule1 > rule2 > rule3 ..."
        } else {
            "Expected a comma separated list of rules that shall not be executed by the cluster command."
        };
        Err(self.error(message, token))
    }

    pub(crate) fn docstring(&mut self, token: Token) -> Option<Expr> {
        let docstring = python::string(self.source, token.range);
        self.defer(docstring)
    }

    /// Keeps the first error in Python code for later, like parser.py does.
    pub(crate) fn defer<T>(&mut self, result: Result<T, Error>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.deferred.get_or_insert(error);
                None
            }
        }
    }

    pub(crate) fn arguments(
        &self,
        read: &Read,
        colon: Token,
        form: Form,
    ) -> Result<Arguments, Error> {
        python::arguments(self.source, read.value, colon.range.end(), form)
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
    pub(crate) fn next_in(&mut self, block: &mut Block) -> Token {
        let token = self.next();
        block.indentation(token);
        token
    }

    /// Continues with the token that ended an inner block, or with the next token.
    pub(crate) fn resume(&mut self, pending: &mut Option<Token>, block: &mut Block) -> Token {
        match pending.take() {
            Some(token) => {
                block.indentation(token);
                token
            }
            None => self.next_in(block),
        }
    }

    /// The end of the statement that `stop` ended: its last token before `stop`.
    pub(crate) fn end_before(&self, stop: Token) -> TextSize {
        if stop.is_trivia() {
            self.last_end
        } else {
            self.previous_end
        }
    }

    pub(crate) fn text(&self, token: Token) -> &'a str {
        &self.source[token.range]
    }

    pub(crate) fn name(&self, token: Token) -> Name {
        Name {
            text: self.text(token).to_string(),
            range: token.range,
        }
    }

    /// Joins the parts of a name pattern such as `module_*` into one name.
    pub(crate) fn joined(&self, parts: &[Token]) -> Option<Name> {
        let (first, last) = (parts.first()?, parts.last()?);
        Some(Name {
            text: parts.iter().map(|part| self.text(*part)).collect(),
            range: first.range.cover(last.range),
        })
    }

    pub(crate) fn error(&self, message: impl Into<String>, token: Token) -> Error {
        Error::new(self.source, message, token.range)
    }

    /// An error where a Python name may clash with a Snakemake keyword.
    pub(crate) fn hinted(&self, message: &str, token: Token, keyword: &str) -> Error {
        let message = format!(
            "{message} The keyword {keyword} has a special meaning in Snakemake. If you named a variable or function like this, please rename it to avoid the conflict."
        );
        self.error(message, token)
    }
}
