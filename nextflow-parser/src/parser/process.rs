//! Parses process and agent definitions: sections, inputs, outputs and their checks.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::section::{LabelOrder, section_block, section_end};
use super::{Parse, Parser};
use crate::ast::{
    Agent, BinaryOperator, BodyKind, Expression, ExpressionKind, Item, Literal, NamedOutput,
    Process, ProcessBody, ProcessInput, Statement, StatementKind, TypedName,
};
use crate::token::TokenKind;
use crate::{Diagnostic, Span};

impl Parser<'_> {
    /// The `processInput` rule: a typed name, `record(...)` or `tuple(...)` of typed names, or a
    /// legacy input statement like `tuple val(meta), path(reads)`.
    pub(super) fn process_input(&mut self) -> Parse<ProcessInput> {
        let start = self.current().span;
        let grouped = matches!(self.peek(), TokenKind::Record | TokenKind::Tuple);
        if grouped && self.nth(1) == TokenKind::LeftParen {
            let tuple = self.at(TokenKind::Tuple);
            if let Some(fields) = self.speculate(Self::field_list) {
                if !tuple {
                    return Ok(ProcessInput::Record(fields));
                }
                if fields.len() < 2 {
                    let message = "Process tuple input must have more than one component";
                    self.error_at(message, self.span_from(start));
                }
                return Ok(ProcessInput::Tuple(fields));
            }
        }
        if let Some(input) = self.speculate(Self::line_name) {
            return Ok(ProcessInput::Named(input));
        }
        Ok(ProcessInput::Statement(self.statement()?))
    }

    /// The typed names of `record(...)` or `tuple(...)`, which must fill the whole line.
    fn field_list(&mut self) -> Parse<Vec<TypedName>> {
        // The keyword and the opening parenthesis.
        self.bump();
        self.bump();
        self.skip_newlines();
        let mut fields = vec![self.typed_name()?];
        while self.eat(TokenKind::Comma) {
            self.skip_newlines();
            if self.at(TokenKind::RightParen) {
                break;
            }
            fields.push(self.typed_name()?);
        }
        self.skip_newlines();
        self.expect(TokenKind::RightParen, "`)`")?;
        self.line_end()?;
        Ok(fields)
    }

    /// A typed name that fills the whole line, like `reads: Path`.
    pub(super) fn line_name(&mut self) -> Parse<TypedName> {
        let name = self.typed_name()?;
        self.line_end()?;
        Ok(name)
    }

    /// The `processOutput` and `workflowEmit` rules: `name`, `name: Type` or `name = value` when
    /// that fills the whole line, else a statement.
    pub(super) fn named_output(&mut self) -> Parse<NamedOutput> {
        let named = self.speculate(|parser| {
            let target = parser.typed_name()?;
            let mut value = None;
            if parser.eat(TokenKind::Assign) {
                value = Some(parser.expression()?);
            }
            parser.line_end()?;
            Ok(NamedOutput::Named { target, value })
        });
        match named {
            Some(output) => Ok(output),
            None => Ok(NamedOutput::Statement(self.statement()?)),
        }
    }

    /// Reports statements that cannot be directives. Like the official `checkDirective`, it
    /// accepts a name, a call without a target like `cpus 4`, and a name minus a value.
    pub(super) fn check_directives(&mut self, statements: &[Statement], message: &str) {
        for statement in statements {
            let valid = match &statement.kind {
                StatementKind::Expression(expression) => match &expression.kind {
                    ExpressionKind::Variable(_) => true,
                    ExpressionKind::MethodCall { target, .. } => target.is_none(),
                    ExpressionKind::Binary {
                        operator: BinaryOperator::Subtract,
                        left,
                        ..
                    } => matches!(left.kind, ExpressionKind::Variable(_)),
                    _ => false,
                },
                StatementKind::Empty => true,
                // Legacy syntax: `name = value` sets a variable of the script, not a directive.
                StatementKind::Assign { target, .. }
                    if matches!(target.kind, ExpressionKind::Variable(_)) =>
                {
                    self.legacy_at(message, statement.span);
                    true
                }
                _ => false,
            };
            if !valid && matches!(statement.kind, StatementKind::If { .. }) {
                // Legacy syntax: an `if` among the directives runs for each task.
                self.legacy.push(Diagnostic::new(message, statement.span));
            } else if !valid {
                self.error_at(message, statement.span);
            }
        }
    }

    /// Declarations and control flow cannot declare inputs or outputs, in legacy or typed
    /// processes alike. Legacy syntax: a variable declaration or assignment there runs when
    /// the process is defined, and later sections see the variable.
    pub(super) fn check_declarations<'s>(
        &mut self,
        statements: impl Iterator<Item = &'s Statement>,
        message: &str,
    ) {
        let spans: Vec<(Span, bool)> = statements
            .filter(|statement| {
                !matches!(
                    statement.kind,
                    StatementKind::Expression(_) | StatementKind::Empty
                )
            })
            .map(|statement| (statement.span, statement.is_variable_statement()))
            .collect();
        for (span, legacy) in spans {
            if legacy {
                self.legacy_at(message, span);
            } else {
                self.error_at(message, span);
            }
        }
    }

    /// Reports topic statements that are not of the form `value >> 'topic'`.
    pub(super) fn check_topics(&mut self, statements: &[Statement]) {
        for statement in statements {
            let valid = match &statement.kind {
                StatementKind::Expression(expression) => matches!(
                    expression.kind,
                    ExpressionKind::Binary {
                        operator: BinaryOperator::RightShift,
                        ..
                    }
                ),
                _ => false,
            };
            if !valid {
                self.error_at("Invalid process topic statement", statement.span);
            }
        }
    }

    /// The `processDef` rule. Sections are read in any order and checked at the end, so that a
    /// misplaced label gives one clear error, as in the official parser.
    pub(super) fn process(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.name()?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let mut process = Process {
            name,
            directives: Vec::new(),
            inputs: Vec::new(),
            stage: Vec::new(),
            outputs: Vec::new(),
            topics: Vec::new(),
            when: None,
            body: None,
            stub: None,
            span: start,
        };
        // The grammar puts a separator before every section, the first one included.
        let mut valid = self.skip_separators() || self.at(TokenKind::RightBrace);
        let mut order = LabelOrder::new();
        let mut previous = None;
        let mut implicit = false;
        let mut last = (0, start);
        let mut out_of_order = false;
        while !self.at_list_end(true) {
            if let Some(section) = self.process_label() {
                // Legacy syntax: the legacy parser takes the sections in any order.
                out_of_order |= !order.accept(section.ranks());
                valid &= !implicit;
                // The label and its colon.
                self.bump();
                self.bump();
                self.skip_newlines();
                last = self.process_section(section, &mut process);
                // Legacy syntax: the legacy parser takes an empty section too.
                out_of_order |= last.0 == 0;
                previous = Some(section);
            } else {
                let block = self.labeled_block(Self::at_process_label);
                last = section_end(&block.statements, start);
                match previous {
                    None => process.directives.extend(block.statements),
                    // Legacy syntax: more statements of `when:`, where the last one is the
                    // condition, when a label follows.
                    Some(ProcessSection::When) if !implicit && self.process_label().is_some() => {
                        let message = "Invalid process definition -- check for missing or \
                                       out-of-order section labels";
                        self.legacy_at(message, block.span);
                        if let Some(Statement {
                            kind: StatementKind::Expression(condition),
                            ..
                        }) = block.statements.last()
                        {
                            process.when = Some(condition.clone());
                        }
                    }
                    // Only `when:` may come right before a body without a label.
                    Some(ProcessSection::When) if !implicit => {
                        implicit = true;
                        let kind = BodyKind::Implicit;
                        process.body = Some(ProcessBody { kind, block });
                    }
                    Some(_) => valid = false,
                }
            }
            self.skip_separators();
        }
        self.expect(TokenKind::RightBrace, "`}`")?;
        process.span = self.span_from(start);
        if previous.is_none() {
            self.implicit_body(&mut process);
        } else {
            self.check_directives(&process.directives, "Invalid process directive");
            let inputs = process.inputs.iter().filter_map(|input| match input {
                ProcessInput::Statement(statement) => Some(statement),
                _ => None,
            });
            self.check_declarations(inputs, "Invalid process input");
            let outputs = process.outputs.iter().filter_map(|output| match output {
                NamedOutput::Statement(statement) => Some(statement),
                NamedOutput::Named { .. } => None,
            });
            self.check_declarations(outputs, "Invalid process output");
        }
        if previous.is_some() && process.body.is_none() {
            // The official parser takes the last statement as the body and then rejects it,
            // unless that would leave the last section empty. Legacy syntax: the legacy parser
            // takes a last string for the script.
            let allowed = previous != Some(ProcessSection::When) && process.stub.is_none();
            let (count, span) = last;
            if let Some(statement) = last_script(&mut process, previous) {
                self.legacy_at(MISSING_LABEL, statement.span);
                let block = section_block(vec![statement], span);
                let kind = BodyKind::Implicit;
                process.body = Some(ProcessBody { kind, block });
            } else if valid && allowed && order.valid[0] && count > 1 {
                self.error_at(MISSING_LABEL, span);
            } else {
                valid = false;
            }
        }
        let message =
            "Invalid process definition -- check for missing or out-of-order section labels";
        if !valid {
            self.error_at(message, process.span);
        } else if out_of_order {
            self.legacy_at(message, process.span);
        }
        Ok(Item::Process(Box::new(process)))
    }

    /// Makes the statements of a process without labels its script body. The official parser
    /// takes all but the last statement as directives, which then need a label. Legacy
    /// syntax: the legacy parser takes them as directives, and the last one as the script.
    fn implicit_body(&mut self, process: &mut Process) {
        let mut statements = std::mem::take(&mut process.directives);
        let Some(last) = statements.pop() else {
            self.error_at("Missing process script body", process.span);
            return;
        };
        if !statements.is_empty() {
            self.legacy_at(MISSING_LABEL, last.span);
            self.check_directives(&statements, "Invalid process directive");
        }
        process.directives = statements;
        let block = section_block(vec![last], process.span);
        let kind = BodyKind::Implicit;
        process.body = Some(ProcessBody { kind, block });
    }

    /// Reads one labelled section into the process. Returns the number of items, which the
    /// grammar requires to be at least one, and the span of the last item.
    fn process_section(&mut self, section: ProcessSection, process: &mut Process) -> (usize, Span) {
        let is_label = Self::at_process_label;
        let start = self.current().span;
        match section {
            ProcessSection::Input => {
                let (inputs, last) = self.item_list(true, is_label, Self::process_input);
                let end = (inputs.len(), last);
                process.inputs.extend(inputs);
                end
            }
            ProcessSection::Stage => {
                let statements = self.section_statements(is_label);
                self.check_directives(&statements, "Invalid stage directive");
                let end = section_end(&statements, start);
                process.stage.extend(statements);
                end
            }
            ProcessSection::Output => {
                let (outputs, last) = self.item_list(true, is_label, Self::named_output);
                let end = (outputs.len(), last);
                process.outputs.extend(outputs);
                end
            }
            ProcessSection::Topic => {
                let statements = self.section_statements(is_label);
                self.check_topics(&statements);
                let end = section_end(&statements, start);
                process.topics.extend(statements);
                end
            }
            ProcessSection::When => {
                let condition = self.expression();
                if condition.is_err() || self.line_end().is_err() {
                    self.skip_section(is_label);
                }
                process.when = condition.ok();
                (1, start)
            }
            ProcessSection::Body(kind) => {
                let block = self.labeled_block(is_label);
                let end = section_end(&block.statements, start);
                process.body = Some(ProcessBody { kind, block });
                end
            }
            ProcessSection::Stub => {
                let block = self.labeled_block(is_label);
                let end = section_end(&block.statements, start);
                process.stub = Some(block);
                end
            }
        }
    }

    /// The process section that starts here, if any, like `input:` or `script:`.
    fn process_label(&self) -> Option<ProcessSection> {
        let section = match self.peek() {
            TokenKind::Input => ProcessSection::Input,
            TokenKind::Stage => ProcessSection::Stage,
            TokenKind::Output => ProcessSection::Output,
            TokenKind::Topic => ProcessSection::Topic,
            TokenKind::When => ProcessSection::When,
            TokenKind::Script => ProcessSection::Body(BodyKind::Script),
            TokenKind::Shell => ProcessSection::Body(BodyKind::Shell),
            TokenKind::Exec => ProcessSection::Body(BodyKind::Exec),
            TokenKind::Stub => ProcessSection::Stub,
            _ => return None,
        };
        self.at_label(self.peek()).then_some(section)
    }

    fn at_process_label(&self) -> bool {
        self.process_label().is_some()
    }

    /// The `agentDef` rule: directives, then `input:`, `output:` and the required `prompt:`.
    pub(super) fn agent(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.name()?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let mut agent = Agent {
            name,
            directives: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            prompt: section_block(Vec::new(), start),
            span: start,
        };
        let mut valid = self.skip_separators() || self.at(TokenKind::RightBrace);
        let mut order = LabelOrder::new();
        let mut labeled = false;
        let mut prompt = false;
        let is_label = Self::at_agent_label;
        while !self.at_list_end(true) {
            let Some(section) = self.agent_label() else {
                valid &= !labeled;
                agent.directives.extend(self.section_statements(is_label));
                continue;
            };
            valid &= order.accept([section as u8]);
            labeled = true;
            // The label and its colon.
            self.bump();
            self.bump();
            self.skip_newlines();
            match section {
                AgentSection::Input => {
                    let (inputs, _) = self.item_list(true, is_label, Self::process_input);
                    valid &= !inputs.is_empty();
                    agent.inputs.extend(inputs);
                }
                AgentSection::Output => {
                    let (outputs, _) = self.item_list(true, is_label, Self::named_output);
                    valid &= !outputs.is_empty();
                    agent.outputs.extend(outputs);
                }
                AgentSection::Prompt => {
                    agent.prompt = self.labeled_block(is_label);
                    valid &= !agent.prompt.statements.is_empty();
                    prompt = true;
                }
            }
            self.skip_separators();
        }
        self.expect(TokenKind::RightBrace, "`}`")?;
        agent.span = self.span_from(start);
        if !labeled && agent.directives.is_empty() {
            self.error_at("Missing agent body", agent.span);
        } else if !valid || !prompt {
            let message =
                "Invalid agent definition -- check for missing or out-of-order section labels";
            self.error_at(message, agent.span);
        } else {
            self.check_directives(&agent.directives, "Invalid agent directive");
        }
        Ok(Item::Agent(Box::new(agent)))
    }

    /// The agent section that starts here, if any.
    fn agent_label(&self) -> Option<AgentSection> {
        let section = match self.peek() {
            TokenKind::Input => AgentSection::Input,
            TokenKind::Output => AgentSection::Output,
            TokenKind::Prompt => AgentSection::Prompt,
            _ => return None,
        };
        self.at_label(self.peek()).then_some(section)
    }

    fn at_agent_label(&self) -> bool {
        self.agent_label().is_some()
    }
}

/// A labelled section of a process body.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessSection {
    Input,
    Stage,
    Output,
    Topic,
    When,
    Body(BodyKind),
    Stub,
}

impl ProcessSection {
    /// Ranks in the two allowed orders: outputs and topics before `when:`, or after `stub:`.
    fn ranks(self) -> [u8; 2] {
        match self {
            ProcessSection::Input => [0, 0],
            ProcessSection::Stage => [1, 1],
            ProcessSection::Output => [2, 5],
            ProcessSection::Topic => [3, 6],
            ProcessSection::When => [4, 2],
            ProcessSection::Body(_) => [5, 3],
            ProcessSection::Stub => [6, 4],
        }
    }
}

/// The last statement of the last section when it is a string, which the legacy parser takes
/// for the script of a process without `script:`, as nf-core/ddamsproteomics has.
fn last_script(process: &mut Process, section: Option<ProcessSection>) -> Option<Statement> {
    let is_string = |statement: &Statement| {
        matches!(
            &statement.kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::Literal(Literal::String(_)) | ExpressionKind::GString(_),
                ..
            })
        )
    };
    match section? {
        ProcessSection::Input => match process.inputs.last() {
            Some(ProcessInput::Statement(statement)) if is_string(statement) => {
                match process.inputs.pop() {
                    Some(ProcessInput::Statement(statement)) => Some(statement),
                    _ => None,
                }
            }
            _ => None,
        },
        ProcessSection::Output => match process.outputs.last() {
            Some(NamedOutput::Statement(statement)) if is_string(statement) => {
                match process.outputs.pop() {
                    Some(NamedOutput::Statement(statement)) => Some(statement),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

pub(super) const MISSING_LABEL: &str =
    "The `script:` or `exec:` label is required when other sections are present";

/// A labelled section of an agent body, in the required order.
#[derive(Clone, Copy)]
pub(super) enum AgentSection {
    Input,
    Output,
    Prompt,
}
