//! Parses type annotations and class names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser, is_name};
use crate::ast::{Name, QualifiedName, Type, TypeArgument};
use crate::token::TokenKind;

impl Parser<'_> {
    /// The `type` rule, like `String`, `List<Path>` or `Map?`.
    pub(super) fn type_annotation(&mut self) -> Parse<Type> {
        let start = self.current().span;
        if self.at(TokenKind::PrimitiveType) {
            let token = self.bump();
            let name = Name {
                text: self.text(token).to_owned(),
                span: token.span,
            };
            return Ok(Type {
                name: QualifiedName { parts: vec![name] },
                arguments: Vec::new(),
                nullable: false,
                dimensions: 0,
                span: token.span,
            });
        }
        let name = self.qualified_class_name()?;
        let arguments = if self.at(TokenKind::Less) {
            self.type_arguments()?
        } else {
            Vec::new()
        };
        let nullable = self.eat(TokenKind::Question);
        Ok(Type {
            name,
            arguments,
            nullable,
            dimensions: 0,
            span: self.span_from(start),
        })
    }

    /// The `legacyType` rule: a type with optional `[]` pairs, as in `String[] names`.
    pub(super) fn legacy_type(&mut self) -> Parse<Type> {
        let start = self.current().span;
        let mut legacy = self.type_annotation()?;
        while self.at(TokenKind::LeftBracket) && self.nth(1) == TokenKind::RightBracket {
            self.bump();
            self.bump();
            legacy.dimensions += 1;
        }
        legacy.span = self.span_from(start);
        Ok(legacy)
    }

    /// The `qualifiedClassName` rule, like `java.util.List`: dotted names ending in a class name.
    pub(super) fn qualified_class_name(&mut self) -> Parse<QualifiedName> {
        let mut parts = Vec::new();
        loop {
            let prefix = is_package_part(self.peek())
                && self.nth(1) == TokenKind::Dot
                && is_package_part(self.nth(2));
            if prefix {
                parts.push(self.bump_name());
                self.bump();
                continue;
            }
            if !self.at(TokenKind::CapitalizedIdentifier) {
                return Err(self.error_here("Expected a class name"));
            }
            parts.push(self.bump_name());
            return Ok(QualifiedName { parts });
        }
    }

    /// `<A, B>`, where an argument may also be the wildcard `?`.
    fn type_arguments(&mut self) -> Parse<Vec<TypeArgument>> {
        self.expect(TokenKind::Less, "'<'")?;
        let mut arguments = Vec::new();
        loop {
            if self.eat(TokenKind::Question) {
                arguments.push(TypeArgument::Wildcard);
            } else {
                arguments.push(TypeArgument::Type(self.type_annotation()?));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Greater, "'>'")?;
        Ok(arguments)
    }

    fn bump_name(&mut self) -> Name {
        let token = self.bump();
        Name {
            text: self.text(token).to_owned(),
            span: token.span,
        }
    }
}

/// The `qualifiedNameElement` rule: names and a few keywords may appear in package names.
fn is_package_part(kind: TokenKind) -> bool {
    is_name(kind) || matches!(kind, TokenKind::As | TokenKind::Def | TokenKind::In)
}
