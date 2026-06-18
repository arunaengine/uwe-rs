//! Parses workflow definitions and the output block.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::declaration::is_expression;
use super::section::{LabelOrder, section_block};
use super::{Parse, Parser, is_name};
use crate::ast::{
    BinaryOperator, ExpressionKind, Item, Literal, Name, NamedOutput, OutputTarget, Statement,
    StatementKind, TypedName, Workflow, WorkflowTake,
};
use crate::token::TokenKind;
use crate::{Diagnostic, Span};

impl Parser<'_> {
    /// Whether `workflow` starts a definition: an optional name, then `{`.
    pub(super) fn at_workflow(&self) -> bool {
        let named = is_name(self.nth(1)) && self.kind_after(2) == TokenKind::LeftBrace;
        named || self.kind_after(1) == TokenKind::LeftBrace
    }

    /// The `workflowDef` rule. As with processes, sections are read in any order and checked at
    /// the end; a body without labels is the main block.
    pub(super) fn workflow(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let mut name = None;
        if self.at_name() {
            name = Some(self.name()?);
        }
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let mut workflow = Workflow {
            name,
            takes: Vec::new(),
            main: section_block(Vec::new(), start),
            emits: Vec::new(),
            publishers: Vec::new(),
            on_complete: None,
            on_error: None,
            span: start,
        };
        // Unlike a main block without label, the first label needs a separator after the brace.
        // Legacy syntax: the legacy parser takes a label on the line of the brace, and the
        // sections in any order.
        let mut legacy = !(self.skip_separators() || self.workflow_label().is_none());
        let mut valid = true;
        let mut order = LabelOrder::new();
        let mut sections = Vec::new();
        let mut implicit = false;
        let mut empty = false;
        while !self.at_list_end(true) {
            let Some(section) = self.workflow_label() else {
                valid &= sections.is_empty() && !implicit;
                implicit = true;
                workflow.main = self.labeled_block(Self::at_workflow_label);
                continue;
            };
            if implicit && sections.is_empty() {
                // Legacy syntax: the statements before the first label are the main block.
                order.accept([WorkflowSection::Main as u8]);
                sections.push(WorkflowSection::Main);
                legacy = true;
            } else {
                valid &= !implicit;
            }
            legacy |= !order.accept([section as u8]);
            let label = self.bump().span;
            self.bump();
            self.skip_newlines();
            empty |= !self.workflow_section(section, label, &mut workflow);
            sections.push(section);
            self.skip_separators();
        }
        self.expect(TokenKind::RightBrace, "`}`")?;
        workflow.span = self.span_from(start);
        let has = |section| sections.contains(&section);
        // Either `main:` or `emit:` is required, and the handlers need `main:`.
        let handlers = has(WorkflowSection::OnComplete) || has(WorkflowSection::OnError);
        let required = has(WorkflowSection::Main) || (has(WorkflowSection::Emit) && !handlers);
        let message =
            "Invalid workflow definition -- check for missing or out-of-order section labels";
        if !valid || (!sections.is_empty() && !required) {
            self.error_at(message, workflow.span);
        } else if empty || legacy {
            // Legacy syntax: a section may be empty, like `take:` right before `main:`.
            self.legacy.push(Diagnostic::new(message, workflow.span));
        }
        Ok(Item::Workflow(Box::new(workflow)))
    }

    /// Reads one labelled section into the workflow and reports sections that the entry or a
    /// named workflow may not have. Returns whether the section has content, as required.
    fn workflow_section(
        &mut self,
        section: WorkflowSection,
        label: Span,
        workflow: &mut Workflow,
    ) -> bool {
        let is_label = Self::at_workflow_label;
        let entry = workflow.name.is_none();
        let start = self.current().span;
        match section {
            WorkflowSection::Take => {
                if entry {
                    self.error_at("Entry workflow cannot have a take section", label);
                }
                let (takes, _) = self.item_list(true, is_label, Self::workflow_take);
                let found = !takes.is_empty();
                workflow.takes.extend(takes);
                found
            }
            WorkflowSection::Main => {
                workflow.main = self.labeled_block(is_label);
                !workflow.main.statements.is_empty()
            }
            WorkflowSection::Emit => {
                if entry {
                    self.error_at("Entry workflow cannot have an emit section", label);
                }
                let (emits, last) = self.item_list(true, is_label, Self::named_output);
                self.check_emits(&emits, start.to(last));
                let found = !emits.is_empty();
                workflow.emits.extend(emits);
                found
            }
            WorkflowSection::Publish => {
                let (publishers, last) = self.item_list(true, is_label, Self::named_output);
                // Legacy syntax: the publish statements of Nextflow 24.10, like `ch >> 'name'`,
                // which named workflows may have too.
                let preview = !publishers.is_empty()
                    && publishers.iter().all(|publisher| {
                        matches!(publisher, NamedOutput::Statement(statement)
                            if preview_publisher(statement).is_some())
                    });
                let publishers: Vec<NamedOutput> = publishers
                    .into_iter()
                    .map(|publisher| {
                        let NamedOutput::Statement(statement) = &publisher else {
                            return publisher;
                        };
                        let message = "Invalid workflow publish statement -- must be an assignment";
                        match preview_publisher(statement) {
                            Some(publisher) => {
                                self.legacy.push(Diagnostic::new(message, statement.span));
                                publisher
                            }
                            None => {
                                self.error_at(message, statement.span);
                                publisher
                            }
                        }
                    })
                    .collect();
                if !entry {
                    let message = "Named workflow cannot have a publish section";
                    match preview {
                        true => self.legacy.push(Diagnostic::new(message, start.to(last))),
                        false => {
                            self.error_at(message, start.to(last));
                        }
                    }
                }
                let found = !publishers.is_empty();
                workflow.publishers.extend(publishers);
                found
            }
            WorkflowSection::OnComplete => {
                let block = self.labeled_block(is_label);
                if !entry {
                    let message = "Named workflow cannot have an onComplete section";
                    self.error_at(message, block.span);
                }
                let found = !block.statements.is_empty();
                workflow.on_complete = Some(block);
                found
            }
            WorkflowSection::OnError => {
                let block = self.labeled_block(is_label);
                if !entry {
                    let message = "Named workflow cannot have an onError section";
                    self.error_at(message, block.span);
                }
                let found = !block.statements.is_empty();
                workflow.on_error = Some(block);
                found
            }
        }
    }

    /// The workflow section that starts here, if any, like `take:` or `main:`.
    fn workflow_label(&self) -> Option<WorkflowSection> {
        let section = match self.peek() {
            TokenKind::Take => WorkflowSection::Take,
            TokenKind::Main => WorkflowSection::Main,
            TokenKind::Emit => WorkflowSection::Emit,
            TokenKind::Publish => WorkflowSection::Publish,
            TokenKind::OnComplete => WorkflowSection::OnComplete,
            TokenKind::OnError => WorkflowSection::OnError,
            _ => return None,
        };
        self.at_label(self.peek()).then_some(section)
    }

    fn at_workflow_label(&self) -> bool {
        self.workflow_label().is_some()
    }

    /// The `workflowTake` rule: a typed name on its own line, else a statement, which is an error.
    fn workflow_take(&mut self) -> Parse<WorkflowTake> {
        if let Some(take) = self.speculate(Self::line_name) {
            return Ok(WorkflowTake::Named(take));
        }
        let statement = self.statement()?;
        self.error_at("Invalid workflow take", statement.span);
        Ok(WorkflowTake::Statement(statement))
    }

    /// Reports emits that are not a name, an assignment or an expression, and unnamed emits
    /// when there are several.
    fn check_emits(&mut self, emits: &[NamedOutput], span: Span) {
        let mut count = 0;
        let mut unnamed = false;
        for emit in emits {
            match emit {
                NamedOutput::Named { .. } => count += 1,
                NamedOutput::Statement(statement) if is_expression(statement) => {
                    count += 1;
                    unnamed = true;
                }
                NamedOutput::Statement(statement) => {
                    let message =
                        "Invalid workflow emit -- must be a name, assignment, or expression";
                    self.error_at(message, statement.span);
                }
            }
        }
        // Legacy syntax: the legacy parser takes several emits without names, which a caller
        // reads by position.
        if unnamed && count > 1 {
            let message = "Every emit must be assigned to a name when there are multiple emits";
            self.legacy.push(Diagnostic::new(message, span));
        }
    }

    /// The `outputDef` rule: `output { ... }` with one target per line.
    pub(super) fn output_block(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let (targets, _) = self.item_list(true, |_| false, Self::output_target);
        self.expect(TokenKind::RightBrace, "`}`")?;
        let span = self.span_from(start);
        Ok(Item::Output { targets, span })
    }

    /// The `outputDeclaration` rule: a name with an optional type, then a block.
    fn output_target(&mut self) -> Parse<OutputTarget> {
        let head = self.speculate(|parser| {
            let target = parser.typed_name()?;
            if !parser.at(TokenKind::LeftBrace) {
                return Err(parser.unexpected());
            }
            Ok(target)
        });
        if let Some(TypedName { name, annotation }) = head {
            let body = self.block()?;
            return Ok(OutputTarget::Target {
                name,
                annotation,
                body,
            });
        }
        // Legacy syntax: the preview output block of Nextflow 24.10 names targets with strings.
        if self.at(TokenKind::StringLiteral) && self.kind_after(1) == TokenKind::LeftBrace {
            let name = self.string_name()?;
            self.legacy
                .push(Diagnostic::new("Invalid output declaration", name.span));
            self.skip_newlines();
            let body = self.block()?;
            return Ok(OutputTarget::Target {
                name,
                annotation: None,
                body,
            });
        }
        let statement = self.statement()?;
        self.error_at("Invalid output declaration", statement.span);
        Ok(OutputTarget::Statement(statement))
    }
}

/// A publish statement of the preview output syntax of Nextflow 24.10, `channel >> 'target'`,
/// as the assignment `target = channel`.
fn preview_publisher(statement: &Statement) -> Option<NamedOutput> {
    let StatementKind::Expression(expression) = &statement.kind else {
        return None;
    };
    let ExpressionKind::Binary {
        operator: BinaryOperator::RightShift,
        left,
        right,
    } = &expression.kind
    else {
        return None;
    };
    let ExpressionKind::Literal(Literal::String(target)) = &right.kind else {
        return None;
    };
    let name = Name {
        text: target.clone(),
        span: right.span,
    };
    Some(NamedOutput::Named {
        target: TypedName {
            name,
            annotation: None,
        },
        value: Some((**left).clone()),
    })
}

/// A labelled section of a workflow body, in the required order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowSection {
    Take,
    Main,
    Emit,
    Publish,
    OnComplete,
    OnError,
}
