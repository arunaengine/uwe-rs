//! Parses script declarations: includes, parameters, processes, workflows and functions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Failed, Parse, Parser, is_name};
use crate::Span;
use crate::ast::{
    Expression, ExpressionKind, Function, Include, IncludeName, Item, Name, ParameterDeclaration,
    Script, Type, TypedName,
};
use crate::token::TokenKind;

impl Parser<'_> {
    /// The `compilationUnit` rule of scripts.
    pub(super) fn script(&mut self) -> Script {
        let (items, _) = self.item_list(false, |_| false, Self::item);
        Script { items }
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
            if !self.at_separator() && !self.at_list_end(nested) {
                // A closing bracket that matches nothing in this list.
                self.bump();
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
