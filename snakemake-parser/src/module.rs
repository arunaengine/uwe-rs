//! Reads `module` and `storage` statements, as parser.py's `Module` and `Storage` do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_text_size::TextRange;

use crate::Error;
use crate::ast::{Module, Storage};
use crate::automaton::{Automaton, Block, Shape, Step};
use crate::keyword::ModuleKeyword;
use crate::python::{self, Form};
use crate::token::{Kind, Token};

impl Automaton<'_> {
    pub(crate) fn module(&mut self, keyword: Token, base: i32) -> Result<(Module, Token), Error> {
        let mut block = Block::new(base);
        let mut name = None;
        let colon = loop {
            let token = self.next_in(&mut block);
            match token.kind {
                Kind::Name => name = Some(self.name(token)),
                Kind::Colon => break token,
                _ => {
                    let message = "Expected name or colon after module keyword.";
                    return Err(self.hinted(message, token, "module"));
                }
            }
        };
        let mut named = name.is_some();
        let mut located = false;
        let mut settings = Vec::new();
        let mut pending = None;
        loop {
            let token = self.resume(&mut pending, &mut block);
            match block.step(token, false) {
                Step::End => {
                    if !named {
                        let message = "Missing module name. A module name must be provided either after the module keyword or inside the module definition after the name keyword.";
                        return Err(self.error(message, colon));
                    }
                    if !located {
                        let message =
                            "A module needs either a path to a Snakefile or a meta wrapper URL.";
                        return Err(self.error(message, colon));
                    }
                    let range = TextRange::new(keyword.range.start(), self.end_before(token));
                    return Ok((
                        Module {
                            name,
                            settings,
                            range,
                        },
                        token,
                    ));
                }
                Step::Skip => continue,
                Step::Content => {}
            }
            match token.kind {
                Kind::Name => {
                    let text = self.text(token);
                    match text {
                        "snakefile" | "meta_wrapper" => located = true,
                        "name" if named => {
                            let message = "Ambiguous module name. A module name was provided directly after the module keyword. Another module name was provided by the name keyword.";
                            return Err(self.hinted(message, token, "module"));
                        }
                        "name" => named = true,
                        _ => {}
                    }
                    let Some(setting) = ModuleKeyword::from_text(text) else {
                        let message = format!("Unexpected keyword {text} in module definition");
                        return Err(self.error(message, token));
                    };
                    let shape = match setting {
                        ModuleKeyword::PathVariables => Shape::Arguments(Form::Plain),
                        _ => Shape::Expression,
                    };
                    let (directive, stop) = self.directive(setting, token, block.inner(), shape)?;
                    settings.push(directive);
                    block.after_newline = true;
                    pending = Some(stop);
                }
                // parser.py drops docstrings of modules.
                Kind::String => {}
                _ => {
                    let message = "Expecting module keyword, comment or docstrings inside a module definition.";
                    return Err(self.error(message, token));
                }
            }
        }
    }

    pub(crate) fn storage(&mut self, keyword: Token, base: i32) -> Result<(Storage, Token), Error> {
        let mut block = Block::new(base);
        let mut tag = None;
        let colon = loop {
            let token = self.next_in(&mut block);
            match token.kind {
                Kind::Name => tag = Some(self.name(token)),
                Kind::Colon => break token,
                _ => {
                    let message = "Expected name or colon after storage keyword.";
                    return Err(self.hinted(message, token, "storage"));
                }
            }
        };
        let read = self.read(block, Shape::Arguments(Form::Storage))?;
        let arguments = self.arguments(&read, colon, Form::Storage);
        let storage = Storage {
            tag,
            arguments: self
                .defer(arguments)
                .unwrap_or_else(|| python::empty(colon.range.end())),
            range: TextRange::new(keyword.range.start(), self.end_before(read.stop)),
        };
        Ok((storage, read.stop))
    }
}
