//! Parses script declarations: includes, parameters, processes, workflows and functions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_name};
use crate::Span;
use crate::ast::{
    Agent, BinaryOperator, Block, BodyKind, Expression, ExpressionKind, Function, Include,
    IncludeName, Item, Name, NamedOutput, OutputTarget, ParameterDeclaration, Process, ProcessBody,
    ProcessInput, Script, Statement, StatementKind, Type, TypedName, Workflow, WorkflowTake,
};
use crate::token::TokenKind;

const MISSING_LABEL: &str =
    "The `script:` or `exec:` label is required when other sections are present";

impl Parser<'_> {
    /// The `compilationUnit` rule of scripts.
    pub(super) fn script(&mut self) -> Script {
        let (items, _) = self.item_list(false, |_| false, Self::item);
        self.check_items(&items);
        Script { items }
    }

    /// Checks that concern the whole script, as in the official `compilationUnit` builder.
    fn check_items(&mut self, items: &[Item]) {
        let mut entry = None;
        let mut output = None;
        let mut parameters = None;
        let mut legacy = false;
        let mut declarations = false;
        for item in items {
            match item {
                Item::Workflow(workflow) if workflow.name.is_none() => {
                    if entry.is_some() {
                        self.error_at("Entry workflow defined more than once", workflow.span);
                    }
                    entry = Some(workflow.span);
                }
                Item::Output { span, .. } => {
                    if output.is_some() {
                        self.error_at("Output block defined more than once", *span);
                    }
                    output = Some(*span);
                }
                Item::Parameters { span, .. } => {
                    if parameters.is_some() {
                        self.error_at("Params block defined more than once", *span);
                    }
                    if legacy {
                        let message =
                            "Params block cannot be mixed with legacy parameter declarations";
                        self.error_at(message, *span);
                    }
                    parameters = Some(*span);
                }
                Item::LegacyParameter { span, .. } => {
                    if parameters.is_some() {
                        let message =
                            "Legacy parameter declarations cannot be mixed with the params block";
                        self.error_at(message, *span);
                    }
                    legacy = true;
                }
                Item::Statement(statement) => {
                    if let Some(name) = definition_name(statement) {
                        let message = format!(
                            "Invalid {name} definition -- check for missing or out-of-order section labels"
                        );
                        self.error_at(message, statement.span);
                        declarations = true;
                    }
                    continue;
                }
                Item::Import { .. } => continue,
                _ => {}
            }
            declarations = true;
        }
        for item in items {
            if let Item::Statement(statement) = item
                && declarations
                && definition_name(statement).is_none()
            {
                let message = "Statements cannot be mixed with script declarations -- move statements into a process, workflow, or function";
                self.error_at(message, statement.span);
            }
        }
        if entry.is_none() {
            if let Some(span) = parameters {
                let message = "Params block cannot be defined without an entry workflow";
                self.error_at(message, span);
            }
            if let Some(span) = output {
                let message = "Output block cannot be defined without an entry workflow";
                self.error_at(message, span);
            }
        }
    }

    /// Items separated by `sep` up to the end of input, a label or, if `nested`, a closing brace,
    /// with the span of the last item. After an error it goes on at the next separator.
    pub(super) fn item_list<T>(
        &mut self,
        nested: bool,
        is_label: fn(&Self) -> bool,
        mut item: impl FnMut(&mut Self) -> Parse<T>,
    ) -> (Vec<T>, Span) {
        let mut items = Vec::new();
        let mut last = self.current().span;
        loop {
            self.skip_separators();
            if self.at_list_end(nested) || is_label(self) {
                return (items, last);
            }
            let start = self.current().span;
            if let Ok(value) = item(self) {
                items.push(value);
                last = self.span_from(start);
                if self.at_separator() || self.at_list_end(nested) {
                    continue;
                }
                self.unexpected();
            }
            self.recover();
            while !self.at_separator() && !self.at_list_end(nested) {
                // A closing bracket that matches nothing in this list.
                self.bump();
                self.recover();
            }
        }
    }

    fn at_list_end(&self, nested: bool) -> bool {
        self.at(TokenKind::EndOfFile) || (nested && self.at(TokenKind::RightBrace))
    }

    /// Reports the current token as out of place, in the wording of the official parser.
    pub(super) fn unexpected(&mut self) -> Failed {
        let token = self.current();
        let message = match token.kind {
            TokenKind::EndOfFile => "Unexpected end of input".to_owned(),
            _ => {
                let text = self.text(token).replace('\r', "\\r").replace('\n', "\\n");
                format!("Unexpected input: '{text}'")
            }
        };
        self.error_at(message, token.span)
    }

    /// Succeeds where a rule that fills a whole line may end.
    fn line_end(&mut self) -> Parse<()> {
        match self.peek() {
            TokenKind::Newline
            | TokenKind::Semicolon
            | TokenKind::RightBrace
            | TokenKind::EndOfFile => Ok(()),
            _ => Err(self.unexpected()),
        }
    }

    /// The kind of the first token at or after offset `n` that is not a newline.
    fn kind_after(&self, mut n: usize) -> TokenKind {
        while self.nth(n) == TokenKind::Newline {
            n += 1;
        }
        self.nth(n)
    }

    /// Whether the keyword here starts a path like `params.a.b` that is followed by `=`.
    fn at_path_assign(&self) -> bool {
        let mut n = 1;
        while self.nth(n) == TokenKind::Dot && is_name(self.nth(n + 1)) {
            n += 2;
        }
        n > 1 && self.kind_after(n) == TokenKind::Assign
    }

    /// Whether the tokens form the `incompleteScriptDeclaration` rule: names joined by dots,
    /// ending in a dot, like `workflow.` while the user is still typing.
    fn at_incomplete(&self) -> bool {
        let mut n = 0;
        while is_name(self.nth(n)) && self.nth(n + 1) == TokenKind::Dot {
            n += 2;
        }
        let end = matches!(
            self.nth(n),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::EndOfFile
        );
        n > 0 && end
    }

    /// The `scriptDeclarationOrStatement` rule, deciding by the leading tokens.
    fn item(&mut self) -> Parse<Item> {
        match self.peek() {
            TokenKind::Nextflow if self.at_path_assign() => self.feature_flag(),
            TokenKind::Include => self.include(),
            TokenKind::Import => self.import(),
            TokenKind::Params if self.kind_after(1) == TokenKind::LeftBrace => {
                self.parameter_block()
            }
            TokenKind::Params if self.at_path_assign() => self.legacy_parameter(),
            TokenKind::Record if is_name(self.nth(1)) => self.record(),
            TokenKind::Enum => self.enumeration(),
            TokenKind::Agent if is_name(self.nth(1)) => self.agent(),
            TokenKind::Process if is_name(self.nth(1)) => self.process(),
            TokenKind::Workflow if self.at_workflow() => self.workflow(),
            TokenKind::Output if self.kind_after(1) == TokenKind::LeftBrace => self.output_block(),
            _ if self.at_incomplete() => self.incomplete(),
            _ => {
                let start = self.current().span;
                match self.speculate(Self::function_head) {
                    Some(head) => self.function(start, head),
                    None => Ok(Item::Statement(self.statement()?)),
                }
            }
        }
    }

    /// The `featureFlagDeclaration` rule, like `nextflow.enable.dsl = 2`.
    fn feature_flag(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.dotted_names()?;
        let value = self.assigned_value()?;
        let span = self.span_from(start);
        if !matches!(value.kind, ExpressionKind::Literal(_)) {
            let message = "Feature flag value must be a literal value (number, string, true/false)";
            self.error_at(message, span);
        }
        Ok(Item::FeatureFlag { name, value, span })
    }

    /// The `(DOT identifier)+` part of feature flags and legacy parameters.
    fn dotted_names(&mut self) -> Parse<Vec<Name>> {
        let mut names = Vec::new();
        while self.eat(TokenKind::Dot) {
            names.push(self.name()?);
        }
        Ok(names)
    }

    /// `nls ASSIGN nls expression`, the value of feature flags and legacy parameters.
    fn assigned_value(&mut self) -> Parse<Expression> {
        self.skip_newlines();
        self.expect(TokenKind::Assign, "`=`")?;
        self.skip_newlines();
        self.expression()
    }

    /// The `includeDeclaration` rule, like `include { FASTQC as QC } from './modules/fastqc'`.
    fn include(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        self.expect(TokenKind::LeftBrace, "`{`")?;
        self.skip_newlines();
        let mut names = Vec::new();
        loop {
            let name = self.name()?;
            let mut alias = None;
            if self.eat(TokenKind::As) {
                alias = Some(self.name()?);
            }
            names.push(IncludeName { name, alias });
            let separated = self.skip_separators();
            if self.eat(TokenKind::RightBrace) {
                break;
            }
            if !separated {
                return Err(self.unexpected());
            }
        }
        self.expect(TokenKind::From, "`from`")?;
        let source = self.string_name()?;
        let span = self.span_from(start);
        Ok(Item::Include(Include {
            names,
            source,
            span,
        }))
    }

    /// The `importDeclaration` rule, which strict syntax keeps only to reject imports clearly.
    fn import(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.qualified_class_name()?;
        let span = self.span_from(start);
        let message = "Groovy `import` declarations are not supported -- use fully-qualified name inline instead";
        self.error_at(message, span);
        Ok(Item::Import { name, span })
    }

    /// The `paramsDef` rule: `params { ... }`.
    fn parameter_block(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let (parameters, _) = self.item_list(true, |_| false, Self::parameter_declaration);
        self.expect(TokenKind::RightBrace, "`}`")?;
        let span = self.span_from(start);
        Ok(Item::Parameters { parameters, span })
    }

    /// The `paramDeclaration` rule: `name: Type = default`, where type and default are optional.
    fn parameter_declaration(&mut self) -> Parse<ParameterDeclaration> {
        let declared = self.speculate(|parser| {
            let name = parser.name()?;
            let annotation = parser.annotation()?;
            let mut default = None;
            if parser.eat(TokenKind::Assign) {
                default = Some(parser.expression()?);
            }
            parser.line_end()?;
            Ok(ParameterDeclaration::Parameter {
                name,
                annotation,
                default,
            })
        });
        if let Some(parameter) = declared {
            return Ok(parameter);
        }
        let statement = self.statement()?;
        self.error_at("Invalid parameter declaration", statement.span);
        Ok(ParameterDeclaration::Statement(statement))
    }

    /// The `paramDeclarationV1` rule: `params.name = value`.
    fn legacy_parameter(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let path = self.dotted_names()?;
        let value = self.assigned_value()?;
        let span = self.span_from(start);
        Ok(Item::LegacyParameter { path, value, span })
    }

    /// The `recordDef` rule: a name and typed fields, one per line.
    fn record(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.name()?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let (fields, _) = self.item_list(true, |_| false, Self::typed_name);
        self.expect(TokenKind::RightBrace, "`}`")?;
        let span = self.span_from(start);
        if fields.is_empty() {
            self.error_at("Missing record body", span);
        }
        for field in fields.iter().filter(|field| field.annotation.is_none()) {
            self.error_at("Missing field type", field.name.span);
        }
        Ok(Item::Record { name, fields, span })
    }

    /// The `enumDef` rule: a name and comma-separated variants.
    fn enumeration(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let name = self.name()?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "`{`")?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while self.at_name() {
            variants.push(self.name()?);
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "`}`")?;
        let span = self.span_from(start);
        Ok(Item::Enum {
            name,
            variants,
            span,
        })
    }

    /// Consumes an `incompleteScriptDeclaration` and reports it; it has no place in the tree.
    fn incomplete(&mut self) -> Parse<Item> {
        let start = self.current().span;
        while !self.at_separator() && !self.at(TokenKind::EndOfFile) {
            self.bump();
        }
        Err(self.error_at("Incomplete declaration", self.span_from(start)))
    }

    /// The start of the `functionDef` rule: `def`, a return type or both, then a name and `(`.
    fn function_head(&mut self) -> Parse<(Option<Type>, Name)> {
        let def = self.eat(TokenKind::Def);
        let mut return_type = None;
        if !def || !self.at_name() || self.nth(1) != TokenKind::LeftParen {
            return_type = Some(self.legacy_type()?);
        }
        let name = self.name()?;
        self.expect(TokenKind::LeftParen, "`(`")?;
        Ok((return_type, name))
    }

    /// The rest of the `functionDef` rule, after the opening parenthesis. Only `def name(...)`
    /// may give its return type after an arrow.
    fn function(&mut self, start: Span, (legacy, name): (Option<Type>, Name)) -> Parse<Item> {
        self.skip_newlines();
        let mut parameters = Vec::new();
        if !self.at(TokenKind::RightParen) {
            parameters = self.parameters()?;
            self.skip_newlines();
        }
        self.expect(TokenKind::RightParen, "`)`")?;
        let mut return_type = legacy;
        if return_type.is_none() && self.eat(TokenKind::Arrow) {
            return_type = Some(self.type_annotation()?);
        }
        self.skip_newlines();
        let body = self.block()?;
        Ok(Item::Function(Box::new(Function {
            name,
            parameters,
            return_type,
            body,
            span: self.span_from(start),
        })))
    }

    /// The `processDef` rule. Sections are read in any order and checked at the end, so that a
    /// misplaced label gives one clear error, as in the official parser.
    fn process(&mut self) -> Parse<Item> {
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
        while !self.at_list_end(true) {
            if let Some(section) = self.process_label() {
                valid &= order.accept(section.ranks()) && !implicit;
                // The label and its colon.
                self.bump();
                self.bump();
                self.skip_newlines();
                last = self.process_section(section, &mut process);
                valid &= last.0 > 0;
                previous = Some(section);
            } else {
                let block = self.labeled_block(Self::at_process_label);
                last = section_end(&block.statements, start);
                match previous {
                    None => process.directives.extend(block.statements),
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
        }
        if previous.is_some() && process.body.is_none() {
            // The official parser takes the last statement as the body and then rejects it,
            // unless that would leave the last section empty.
            let allowed = previous != Some(ProcessSection::When) && process.stub.is_none();
            let (count, span) = last;
            if valid && allowed && order.valid[0] && count > 1 {
                self.error_at(MISSING_LABEL, span);
            } else {
                valid = false;
            }
        }
        if !valid {
            let message =
                "Invalid process definition -- check for missing or out-of-order section labels";
            self.error_at(message, process.span);
        }
        Ok(Item::Process(Box::new(process)))
    }

    /// Makes the statements of a process without labels its script body. The official parser
    /// takes all but the last statement as directives, which then need a label.
    fn implicit_body(&mut self, process: &mut Process) {
        let statements = std::mem::take(&mut process.directives);
        let Some(last) = statements.last() else {
            self.error_at("Missing process script body", process.span);
            return;
        };
        if statements.len() > 1 {
            self.error_at(MISSING_LABEL, last.span);
        }
        let block = section_block(statements, process.span);
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
        if self.nth(1) != TokenKind::Colon {
            return None;
        }
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
        Some(section)
    }

    fn at_process_label(&self) -> bool {
        self.process_label().is_some()
    }

    /// The `processInput` rule: a typed name, `record(...)` or `tuple(...)` of typed names, or a
    /// legacy input statement like `tuple val(meta), path(reads)`.
    fn process_input(&mut self) -> Parse<ProcessInput> {
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
    fn line_name(&mut self) -> Parse<TypedName> {
        let name = self.typed_name()?;
        self.line_end()?;
        Ok(name)
    }

    /// The `processOutput` and `workflowEmit` rules: `name`, `name: Type` or `name = value` when
    /// that fills the whole line, else a statement.
    fn named_output(&mut self) -> Parse<NamedOutput> {
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

    /// The `agentDef` rule: directives, then `input:`, `output:` and the required `prompt:`.
    fn agent(&mut self) -> Parse<Item> {
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
        if self.nth(1) != TokenKind::Colon {
            return None;
        }
        match self.peek() {
            TokenKind::Input => Some(AgentSection::Input),
            TokenKind::Output => Some(AgentSection::Output),
            TokenKind::Prompt => Some(AgentSection::Prompt),
            _ => None,
        }
    }

    fn at_agent_label(&self) -> bool {
        self.agent_label().is_some()
    }

    /// Whether `workflow` starts a definition: an optional name, then `{`.
    fn at_workflow(&self) -> bool {
        let named = is_name(self.nth(1)) && self.kind_after(2) == TokenKind::LeftBrace;
        named || self.kind_after(1) == TokenKind::LeftBrace
    }

    /// The `workflowDef` rule. As with processes, sections are read in any order and checked at
    /// the end; a body without labels is the main block.
    fn workflow(&mut self) -> Parse<Item> {
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
        let mut valid = self.skip_separators() || self.workflow_label().is_none();
        let mut order = LabelOrder::new();
        let mut sections = Vec::new();
        let mut implicit = false;
        while !self.at_list_end(true) {
            let Some(section) = self.workflow_label() else {
                valid &= sections.is_empty() && !implicit;
                implicit = true;
                workflow.main = self.labeled_block(Self::at_workflow_label);
                continue;
            };
            valid &= order.accept([section as u8]) && !implicit;
            let label = self.bump().span;
            self.bump();
            self.skip_newlines();
            valid &= self.workflow_section(section, label, &mut workflow);
            sections.push(section);
            self.skip_separators();
        }
        self.expect(TokenKind::RightBrace, "`}`")?;
        workflow.span = self.span_from(start);
        let has = |section| sections.contains(&section);
        // Either `main:` or `emit:` is required, and the handlers need `main:`.
        let handlers = has(WorkflowSection::OnComplete) || has(WorkflowSection::OnError);
        let required = has(WorkflowSection::Main) || (has(WorkflowSection::Emit) && !handlers);
        if !valid || (!sections.is_empty() && !required) {
            let message =
                "Invalid workflow definition -- check for missing or out-of-order section labels";
            self.error_at(message, workflow.span);
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
                for publisher in &publishers {
                    if let NamedOutput::Statement(statement) = publisher {
                        let message = "Invalid workflow publish statement -- must be an assignment";
                        self.error_at(message, statement.span);
                    }
                }
                if !entry {
                    let message = "Named workflow cannot have a publish section";
                    self.error_at(message, start.to(last));
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
        if self.nth(1) != TokenKind::Colon {
            return None;
        }
        match self.peek() {
            TokenKind::Take => Some(WorkflowSection::Take),
            TokenKind::Main => Some(WorkflowSection::Main),
            TokenKind::Emit => Some(WorkflowSection::Emit),
            TokenKind::Publish => Some(WorkflowSection::Publish),
            TokenKind::OnComplete => Some(WorkflowSection::OnComplete),
            TokenKind::OnError => Some(WorkflowSection::OnError),
            _ => None,
        }
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
        if unnamed && count > 1 {
            let message = "Every emit must be assigned to a name when there are multiple emits";
            self.error_at(message, span);
        }
    }

    /// The `outputDef` rule: `output { ... }` with one target per line.
    fn output_block(&mut self) -> Parse<Item> {
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
        let statement = self.statement()?;
        self.error_at("Invalid output declaration", statement.span);
        Ok(OutputTarget::Statement(statement))
    }

    /// The statements of a section as a block, like the body after `script:`.
    fn labeled_block(&mut self, is_label: fn(&Self) -> bool) -> Block {
        let start = self.current().span;
        section_block(self.section_statements(is_label), start)
    }

    /// The statements of one section, up to the next label or the closing brace.
    fn section_statements(&mut self, is_label: fn(&Self) -> bool) -> Vec<Statement> {
        let statements = self.statement_sequence(is_label).unwrap_or_default();
        if !self.at_list_end(true) && !is_label(self) {
            // The sequence stopped early after an error; go on at the next label.
            self.skip_section(is_label);
        }
        statements
    }

    /// Skips to the next section label or the closing brace, after an error in a section.
    fn skip_section(&mut self, is_label: fn(&Self) -> bool) {
        loop {
            self.recover();
            if self.skip_separators() {
                if is_label(self) {
                    return;
                }
            } else if self.at_list_end(true) {
                return;
            } else {
                // A closing bracket that matches nothing in this section.
                self.bump();
            }
        }
    }

    /// Reports statements that cannot be directives. Like the official `checkDirective`, it
    /// accepts a name, a call without a target like `cpus 4`, and a name minus a value.
    fn check_directives(&mut self, statements: &[Statement], message: &str) {
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
                _ => false,
            };
            if !valid {
                self.error_at(message, statement.span);
            }
        }
    }

    /// Reports topic statements that are not of the form `value >> 'topic'`.
    fn check_topics(&mut self, statements: &[Statement]) {
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

    /// The `nameTypePair` rule: `name` or `name: Type`.
    fn typed_name(&mut self) -> Parse<TypedName> {
        let name = self.name()?;
        let annotation = self.annotation()?;
        Ok(TypedName { name, annotation })
    }

    /// An optional `: Type`.
    fn annotation(&mut self) -> Parse<Option<Type>> {
        if !self.eat(TokenKind::Colon) {
            return Ok(None);
        }
        self.type_annotation().map(Some)
    }
}

/// A labelled section of a process body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcessSection {
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

/// A labelled section of a workflow body, in the required order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkflowSection {
    Take,
    Main,
    Emit,
    Publish,
    OnComplete,
    OnError,
}

/// A labelled section of an agent body, in the required order.
#[derive(Clone, Copy)]
enum AgentSection {
    Input,
    Output,
    Prompt,
}

/// Checks section labels against one or more allowed orders, given as a rank per label.
struct LabelOrder<const N: usize> {
    last: [Option<u8>; N],
    valid: [bool; N],
}

impl<const N: usize> LabelOrder<N> {
    fn new() -> Self {
        Self {
            last: [None; N],
            valid: [true; N],
        }
    }

    /// Records the next label and tells whether some order still allows all labels so far.
    fn accept(&mut self, ranks: [u8; N]) -> bool {
        for (index, rank) in ranks.into_iter().enumerate() {
            if self.last[index].is_some_and(|last| last >= rank) {
                self.valid[index] = false;
            }
            self.last[index] = Some(rank);
        }
        self.valid.contains(&true)
    }
}

/// A block made of the statements of a section, spanning from the first to the last statement,
/// or empty at `start` when there are none.
fn section_block(statements: Vec<Statement>, start: Span) -> Block {
    let span = match (statements.first(), statements.last()) {
        (Some(first), Some(last)) => first.span.to(last.span),
        _ => Span {
            start: start.start,
            end: start.start,
        },
    };
    Block { statements, span }
}

/// The number of statements in a section and the span of the last one.
fn section_end(statements: &[Statement], start: Span) -> (usize, Span) {
    let last = statements.last().map_or(start, |statement| statement.span);
    (statements.len(), last)
}

/// Whether a statement is an expression other than a bare name: an emit without a name.
fn is_expression(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::Expression(expression) => {
            !matches!(expression.kind, ExpressionKind::Variable(_))
        }
        _ => false,
    }
}

/// The name of a statement like `process { ... }` that looks like a definition but did not parse
/// as one, which the official parser reports as an invalid definition.
fn definition_name(statement: &Statement) -> Option<&str> {
    let StatementKind::Expression(expression) = &statement.kind else {
        return None;
    };
    let ExpressionKind::MethodCall { name, .. } = &expression.kind else {
        return None;
    };
    let name = name.text.as_str();
    ["agent", "process", "workflow", "output"]
        .contains(&name)
        .then_some(name)
}
