//! Reads rules, checkpoints and `use rule` statements, as parser.py's `Rule` and `UseRule` do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_text_size::TextRange;

use crate::Error;
use crate::ast::{Rule, UseRule};
use crate::automaton::{Automaton, Block, Shape, Step};
use crate::keyword::RuleKeyword;
use crate::python::Form;
use crate::token::{Kind, Token};

/// `use rule` header states of parser.py's `UseRule`.
#[derive(Clone, Copy)]
enum Header {
    Keyword,
    Rules,
    RulesEnd,
    RulesNext,
    From,
    Modifier,
    Alias,
    Exclude,
    ExcludeNext,
    With,
}

impl Automaton<'_> {
    pub(crate) fn rule(&mut self, keyword: Token, base: i32) -> Result<(Rule, Token), Error> {
        let mut block = Block::new(base);
        let mut name = None;
        loop {
            let token = self.next_in(&mut block);
            match token.kind {
                Kind::Name => name = Some(self.name(token)),
                Kind::Colon => break,
                _ => {
                    let message = "Expected name or colon after rule or checkpoint keyword.";
                    return Err(self.hinted(message, token, "rule"));
                }
            }
        }
        let mut rule = Rule {
            checkpoint: self.text(keyword) == "checkpoint",
            name,
            docstrings: Vec::new(),
            directives: Vec::new(),
            range: keyword.range,
        };
        let mut executed = false;
        let mut pending = None;
        loop {
            let token = self.resume(&mut pending, &mut block);
            match block.step(token, false) {
                Step::End => {
                    rule.range = TextRange::new(keyword.range.start(), self.end_before(token));
                    return Ok((rule, token));
                }
                Step::Skip => continue,
                Step::Content => {}
            }
            match token.kind {
                Kind::Name => {
                    let keyword = self.rule_keyword(token, &rule, &mut executed)?;
                    let shape = match keyword {
                        RuleKeyword::Run => Shape::Run,
                        RuleKeyword::Shell => Shape::Arguments(Form::Shell),
                        keyword if keyword.is_execution() => Shape::Arguments(Form::Command),
                        _ => Shape::Arguments(Form::Plain),
                    };
                    let (directive, stop) = self.directive(keyword, token, block.inner(), shape)?;
                    rule.directives.push(directive);
                    block.after_newline = true;
                    pending = Some(stop);
                }
                Kind::String => {
                    rule.docstrings.extend(self.docstring(token));
                    block.after_newline = false;
                }
                _ => {
                    let message =
                        "Expecting rule keyword, comment or docstrings inside a rule definition.";
                    return Err(self.error(message, token));
                }
            }
        }
    }

    /// Looks up a directive of a rule, which must end with at most one execution directive.
    fn rule_keyword(
        &self,
        token: Token,
        rule: &Rule,
        executed: &mut bool,
    ) -> Result<RuleKeyword, Error> {
        let text = self.text(token);
        let keyword = RuleKeyword::from_text(text);
        let name = rule.name.as_ref().map_or("", |name| name.text.as_str());
        if keyword.is_some_and(RuleKeyword::is_execution) {
            if *executed {
                let message = format!("Multiple execution keywords in rule {name}.");
                return Err(self.error(message, token));
            }
            *executed = true;
        } else if *executed {
            let message =
                format!("No rule keywords allowed after the execution keyword in rule {name}.");
            return Err(self.error(message, token));
        }
        self.property(token, keyword)
    }

    /// Rejects `version`, which Snakemake 8 removed, and unknown directives.
    fn property(&self, token: Token, keyword: Option<RuleKeyword>) -> Result<RuleKeyword, Error> {
        let text = self.text(token);
        if text == "version" {
            let message =
                "Keyword version is deprecated. Use conda or container directive instead.";
            return Err(self.error(message, token));
        }
        keyword.ok_or_else(|| {
            self.error(
                format!("Unexpected keyword {text} in rule definition"),
                token,
            )
        })
    }

    pub(crate) fn use_rule(
        &mut self,
        keyword: Token,
        base: i32,
    ) -> Result<(UseRule, Token), Error> {
        let mut block = Block::new(base);
        let mut rule = UseRule {
            rules: Vec::new(),
            module: None,
            exclude: Vec::new(),
            alias: None,
            docstrings: Vec::new(),
            with: None,
            range: keyword.range,
        };
        let mut alias: Vec<Token> = Vec::new();
        let mut state = Header::Keyword;
        loop {
            let token = self.next_in(&mut block);
            let text = self.text(token);
            let name = token.kind == Kind::Name;
            let ends = matches!(token.kind, Kind::Newline | Kind::Comment | Kind::EndOfFile);
            state = match state {
                Header::Keyword if name && text == "rule" => Header::Rules,
                Header::Keyword => {
                    return Err(self.error("Expecting keyword 'rule' after keyword 'use'", token));
                }
                Header::Rules if name => {
                    if text == "from" || (text == "as" && rule.rules.is_empty()) {
                        let message = "Expecting rule names after 'use rule' statement.";
                        return Err(self.error(message, token));
                    }
                    rule.rules.push(self.name(token));
                    Header::RulesNext
                }
                Header::Rules if token.kind == Kind::Star => {
                    rule.rules.push(self.name(token));
                    Header::RulesEnd
                }
                Header::Rules => {
                    let message = "Expecting rule name or '*' after 'use rule' statement.";
                    return Err(self.error(message, token));
                }
                Header::RulesEnd if name && text == "from" => Header::From,
                Header::RulesEnd => {
                    let message = "Expecting list of rules in 'use rule' statement to end with keyword 'from'.";
                    return Err(self.error(message, token));
                }
                Header::RulesNext if name && text == "from" => Header::From,
                Header::RulesNext if name && text == "as" => Header::Alias,
                Header::RulesNext if token.kind == Kind::Comma => Header::Rules,
                Header::RulesNext => {
                    let message = "Expecting list of rules in 'use rule' statement to end with keyword 'from'.";
                    return Err(self.error(message, token));
                }
                Header::From if name => {
                    rule.module = Some(self.name(token));
                    Header::Modifier
                }
                Header::From => {
                    let message =
                        "Expecting module name after 'from' keyword in 'use rule' statement.";
                    return Err(self.error(message, token));
                }
                Header::Modifier if name && text == "as" => Header::Alias,
                Header::Modifier if name && text == "exclude" => Header::Exclude,
                Header::Modifier | Header::Alias if name && text == "with" => {
                    if rule.rules.iter().any(|name| name.text == "*") {
                        let message = "Keyword 'with' in 'use rule' statement is not allowed in combination with rule pattern '*'.";
                        return Err(self.error(message, token));
                    }
                    Header::With
                }
                Header::Alias if name || token.kind == Kind::Star => {
                    alias.push(token);
                    Header::Alias
                }
                Header::Modifier | Header::Alias if ends => {
                    rule.alias = self.joined(&alias);
                    rule.range = TextRange::new(keyword.range.start(), self.end_before(token));
                    return Ok((rule, token));
                }
                Header::Modifier => {
                    let message = "Expecting 'as', 'exclude', 'with' or the end of the line in 'use rule' statement.";
                    return Err(self.error(message, token));
                }
                Header::Alias => {
                    let message = "Expecting rulename modifying pattern (e.g. modulename_*) after 'as' keyword.";
                    return Err(self.error(message, token));
                }
                Header::Exclude if name => {
                    rule.exclude.push(self.name(token));
                    Header::ExcludeNext
                }
                Header::Exclude => {
                    let message =
                        "Expecting rule name(s) after 'exclude' keyword in 'use rule' statement.";
                    return Err(self.error(message, token));
                }
                Header::ExcludeNext if name && text == "from" => Header::From,
                Header::ExcludeNext if name && text == "as" => Header::Alias,
                // parser.py skips other names here and drops any other token.
                Header::ExcludeNext if name => Header::ExcludeNext,
                Header::ExcludeNext if token.kind == Kind::Comma => Header::Exclude,
                Header::ExcludeNext => Header::Modifier,
                Header::With if token.kind == Kind::Colon => break,
                Header::With => {
                    let message = "Expecting colon after 'with' keyword in 'use rule' statement.";
                    return Err(self.error(message, token));
                }
            };
        }
        rule.alias = self.joined(&alias);
        let mut directives = Vec::new();
        let mut pending = None;
        loop {
            let token = self.resume(&mut pending, &mut block);
            match block.step(token, false) {
                Step::End => {
                    rule.with = Some(directives);
                    rule.range = TextRange::new(keyword.range.start(), self.end_before(token));
                    return Ok((rule, token));
                }
                Step::Skip => continue,
                Step::Content => {}
            }
            // The directives of a `with:` block leave parser.py's output of this block unchanged.
            match token.kind {
                Kind::Name => {
                    let keyword = RuleKeyword::from_text(self.text(token))
                        .filter(|keyword| !keyword.is_execution());
                    let keyword = self.property(token, keyword)?;
                    let shape = Shape::Arguments(Form::Plain);
                    let (directive, stop) = self.directive(keyword, token, block.inner(), shape)?;
                    directives.push(directive);
                    pending = Some(stop);
                }
                Kind::String => rule.docstrings.extend(self.docstring(token)),
                _ => {
                    let message = "Expecting rule keyword, comment or docstrings inside a 'use rule ... with:' statement.";
                    return Err(self.error(message, token));
                }
            }
        }
    }
}
