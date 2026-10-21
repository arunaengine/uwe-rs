//! Parses the script level: includes, parameters, records, enums, functions and statements.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_name};
use crate::ast::{
    AssignOperator, BinaryOperator, Expression, ExpressionKind, Function, GStringPart, Include,
    IncludeName, IncludeParams, Item, Literal, Name, ParameterDeclaration, Script, Statement,
    StatementKind, Type, TypedName,
};
use crate::token::TokenKind;
use crate::{Diagnostic, Span};

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
        // Legacy syntax: the legacy parser runs these statements when the script loads.
        for item in items {
            if let Item::Statement(statement) = item
                && declarations
                && definition_name(statement).is_none()
            {
                let message = "Statements cannot be mixed with script declarations -- move statements into a process, workflow, or function";
                self.legacy.push(Diagnostic::new(message, statement.span));
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

    pub(super) fn at_list_end(&self, nested: bool) -> bool {
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
    pub(super) fn line_end(&mut self) -> Parse<()> {
        match self.peek() {
            TokenKind::Newline
            | TokenKind::Semicolon
            | TokenKind::RightBrace
            | TokenKind::EndOfFile => Ok(()),
            _ => Err(self.unexpected()),
        }
    }

    /// The kind of the first token at or after offset `n` that is not a newline.
    pub(super) fn kind_after(&self, mut n: usize) -> TokenKind {
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

    /// Whether a declaration keyword starts a statement instead, because the declaration could
    /// not end there: `params { v -> v }` calls `params`, and so does `workflow { it } + 1`.
    fn at_call_statement(&mut self) -> bool {
        let named = match self.peek() {
            TokenKind::Params | TokenKind::Output => false,
            TokenKind::Workflow => is_name(self.nth(1)),
            TokenKind::Record | TokenKind::Process if is_name(self.nth(1)) => true,
            _ => return false,
        };
        let mut brace = if named { 2 } else { 1 };
        while self.nth(brace) == TokenKind::Newline {
            brace += 1;
        }
        if self.nth(brace) != TokenKind::LeftBrace {
            return false;
        }
        // Other keywords with a closure make invalid definitions either way.
        let keyword = matches!(self.peek(), TokenKind::Params | TokenKind::Record);
        let closure = keyword
            && self.lookahead(|parser| {
                parser.position += brace;
                let closure = parser.closure(true)?;
                closure.parameters.map(|_| ()).ok_or(Failed)
            });
        let (mut end, mut depth) = (brace, 0usize);
        loop {
            match self.nth(end) {
                TokenKind::LeftBrace | TokenKind::InterpolationStart => depth += 1,
                TokenKind::RightBrace if depth == 1 => break,
                TokenKind::RightBrace => depth -= 1,
                TokenKind::EndOfFile => return closure,
                _ => {}
            }
            end += 1;
        }
        let ends = matches!(
            self.nth(end + 1),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::EndOfFile
        );
        closure || !ends
    }

    /// The `scriptDeclarationOrStatement` rule, deciding by the leading tokens.
    fn item(&mut self) -> Parse<Item> {
        if self.at_call_statement() {
            return Ok(Item::Statement(self.statement()?));
        }
        match self.peek() {
            TokenKind::Nextflow if self.at_path_assign() => self.feature_flag(),
            TokenKind::Include => Ok(Item::Include(self.include()?)),
            TokenKind::Import => self.legacy_import(),
            _ if self.at_class() => self.class_item(),
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
                    None => {
                        let statement = self.statement()?;
                        self.remember_include_variable(&statement);
                        Ok(Item::Statement(statement))
                    }
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
    pub(super) fn include(&mut self) -> Parse<Include> {
        let start = self.bump().span;
        self.expect(TokenKind::LeftBrace, "`{`")?;
        self.skip_newlines();
        let mut names = Vec::new();
        loop {
            let name = self.name()?;
            let mut alias = None;
            // Legacy syntax: a line break before `as`.
            if self.at(TokenKind::Newline) && self.kind_after(0) == TokenKind::As {
                let span = self.current().span;
                self.legacy_at("Unexpected input: 'as'", span);
                self.skip_newlines();
            }
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
        let source = match self.peek() {
            TokenKind::StringLiteral if self.nth(1) != TokenKind::Add => self.string_name()?,
            _ => self.legacy_include_source()?,
        };
        // Legacy syntax: `addParams(...)` or `params(...)` gives the included script params.
        let params = match self.at_name() && self.nth(1) == TokenKind::LeftParen {
            true => {
                let name = self.name()?;
                if !matches!(name.text.as_str(), "addParams" | "params") {
                    let message = format!("Unexpected input: '{}'", name.text);
                    return Err(self.error_at(message, name.span));
                }
                self.legacy_at(format!("Unexpected input: '{}'", name.text), name.span);
                let arguments = self.arguments()?;
                Some(IncludeParams {
                    add: name.text == "addParams",
                    arguments,
                    span: self.span_from(name.span),
                })
            }
            false => None,
        };
        let span = self.span_from(start);
        Ok(Include {
            names,
            source,
            params,
            span,
        })
    }

    /// Legacy syntax: an include source made of strings and the directories of the script,
    /// like `"${projectDir}/modules/x"` or `PROJECT_DIR + '/x.nf'`. The text keeps `${name}`
    /// for each directory, which the loader fills.
    fn legacy_include_source(&mut self) -> Parse<Name> {
        let start = self.current().span;
        let expression = self.expression()?;
        let span = self.span_from(start);
        let Some(text) = self.include_text(&expression) else {
            return Err(self.error_at("Expected a string", span));
        };
        self.legacy_at("Expected a string", span);
        Ok(Name { text, span })
    }

    /// The include source text of an expression, if it is made of strings, the variables of
    /// the script's directories, `workflow.projectDir` and remembered script variables.
    fn include_text(&self, expression: &Expression) -> Option<String> {
        match &expression.kind {
            ExpressionKind::Literal(Literal::String(text)) => Some(text.clone()),
            ExpressionKind::GString(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        GStringPart::Text(part) => text.push_str(part),
                        GStringPart::Value(value) => text.push_str(&self.include_text(value)?),
                    }
                }
                Some(text)
            }
            ExpressionKind::Variable(name) if INCLUDE_VARIABLES.contains(&name.as_str()) => {
                Some(format!("${{{name}}}"))
            }
            ExpressionKind::Variable(name) => self.include_variables.get(name).cloned(),
            ExpressionKind::Property { target, name, .. }
                if matches!(&target.kind, ExpressionKind::Variable(workflow) if workflow == "workflow")
                    && matches!(name.text.as_str(), "projectDir" | "launchDir") =>
            {
                Some(format!("${{{}}}", name.text))
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                left,
                right,
            } => Some(self.include_text(left)? + &self.include_text(right)?),
            // `file(text)`, whose text names the file.
            ExpressionKind::MethodCall {
                target: None,
                name,
                arguments,
                ..
            } if name.text == "file" && arguments.len() == 1 && arguments[0].name.is_none() => {
                self.include_text(&arguments[0].value)
            }
            _ => None,
        }
    }

    /// Remembers a script variable that a later include source may use, like
    /// `PROJECT_DIR = workflow.projectDir` or `def local_modules = file("$baseDir/x.nf")`.
    fn remember_include_variable(&mut self, statement: &Statement) {
        let (name, value) = match &statement.kind {
            StatementKind::Assign {
                target,
                operator: AssignOperator::Assign,
                value,
            } => match &target.kind {
                ExpressionKind::Variable(name) => (name, value),
                _ => return,
            },
            StatementKind::Declaration {
                names,
                initializer: Some(value),
                destructure: false,
                ..
            } if names.len() == 1 => (&names[0].name.text, value),
            _ => return,
        };
        if let Some(text) = self.include_text(value) {
            self.include_variables.insert(name.clone(), text);
        }
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
    pub(super) fn function_head(&mut self) -> Parse<(Option<Type>, Name)> {
        // Legacy syntax: modifiers like `public static`, which change nothing for a function of
        // a script.
        while self.at(TokenKind::Identifier) && super::class::MODIFIERS.contains(&self.word(0)) {
            let token = self.bump();
            let message = format!(
                "`{}` is not allowed as an identifier because it is a Groovy keyword",
                self.text(token)
            );
            self.legacy_at(message, token.span);
        }
        let def = self.eat(TokenKind::Def);
        let mut return_type = None;
        // Legacy syntax: `void name(...)`, a function without a result.
        if !def
            && self.at_name()
            && self.text(self.current()) == "void"
            && is_name(self.nth(1))
            && self.nth(2) == TokenKind::LeftParen
        {
            let token = self.bump();
            self.legacy_at(
                "`void` is not allowed as an identifier because it is a Groovy keyword",
                token.span,
            );
            let void = Name {
                text: "void".into(),
                span: token.span,
            };
            return_type = Some(Type {
                name: crate::ast::QualifiedName { parts: vec![void] },
                arguments: Vec::new(),
                nullable: false,
                dimensions: 0,
                span: token.span,
            });
        } else if !def || !self.at_name() || self.nth(1) != TokenKind::LeftParen {
            return_type = Some(self.legacy_type()?);
        }
        let name = self.name()?;
        self.expect(TokenKind::LeftParen, "`(`")?;
        Ok((return_type, name))
    }

    /// The rest of the `functionDef` rule, after the opening parenthesis. Only `def name(...)`
    /// may give its return type after an arrow.
    pub(super) fn function(
        &mut self,
        start: Span,
        (legacy, name): (Option<Type>, Name),
    ) -> Parse<Item> {
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

    /// The `nameTypePair` rule: `name` or `name: Type`.
    pub(super) fn typed_name(&mut self) -> Parse<TypedName> {
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

/// Whether a statement is an expression other than a bare name: an emit without a name.
pub(super) fn is_expression(statement: &Statement) -> bool {
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

/// The variables that a legacy include source may use: the directories of the run and of the
/// including script.
pub const INCLUDE_VARIABLES: &[&str] = &["projectDir", "baseDir", "launchDir", "moduleDir"];
