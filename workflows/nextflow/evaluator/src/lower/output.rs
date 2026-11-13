//! The declarations of the `output { }` block: each workflow output with its options, like
//! `path 'fastq'` or `index { path 'samples.csv' }`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::ast::{self, ExpressionKind as Syntax};

use super::Lowering;
use super::process::call;
use crate::lowered::{Code, OutputDeclaration};

impl Lowering<'_> {
    /// A workflow output and its options; `index` takes a section of options of its own.
    pub(super) fn output_declaration(
        &mut self,
        name: &str,
        body: &ast::Block,
    ) -> OutputDeclaration {
        let mut declaration = OutputDeclaration {
            name: name.into(),
            options: Vec::new(),
            index: Vec::new(),
        };
        for statement in &body.statements {
            let Some((option, arguments, span)) = call(statement) else {
                self.error("Invalid workflow output option", statement.span);
                continue;
            };
            let Some(argument) = arguments.first() else {
                self.error(format!("The option `{option}` needs a value"), span);
                continue;
            };
            match (option, &argument.value.kind) {
                ("index", Syntax::Closure(closure)) => {
                    declaration.index = self.options(&closure.body);
                }
                _ => {
                    let code = self.expression_code(option, &argument.value, span);
                    declaration.options.push((option.into(), Rc::new(code)));
                }
            }
        }
        declaration
    }

    /// The options of a section like `index { }`: each call with its value.
    fn options(&mut self, body: &ast::Block) -> Vec<(Rc<str>, Rc<Code>)> {
        let mut options = Vec::new();
        for statement in &body.statements {
            match call(statement) {
                Some((option, [argument, ..], span)) => {
                    let code = self.expression_code(option, &argument.value, span);
                    options.push((option.into(), Rc::new(code)));
                }
                _ => self.error("Invalid workflow output option", statement.span),
            }
        }
        options
    }
}
