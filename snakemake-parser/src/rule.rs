//! Reads rules and checkpoints, as parser.py's `Rule` does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_text_size::TextRange;

use crate::Error;
use crate::ast::Rule;
use crate::automaton::{Automaton, Block, Shape, Step};
use crate::keyword::RuleKeyword;
use crate::python::Form;
use crate::token::{Kind, Token};

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
}
