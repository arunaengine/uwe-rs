//! Parses Groovy classes and imports, which only the legacy parser accepts. Older nf-core
//! pipelines keep helper classes like `WorkflowMain` in `lib/*.groovy`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Parse, Parser, is_name};
use crate::ast::{Class, Function, Item, Member, Name, Parameter, QualifiedName};
use crate::token::TokenKind;

/// Modifiers of classes and their members, which change nothing here but `static`.
pub(super) const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "final",
    "abstract",
    "synchronized",
    "transient",
    "volatile",
];

impl Parser<'_> {
    /// Whether a class declaration starts here: modifiers, then `class` or `enum` and a name.
    pub(super) fn at_class(&self) -> bool {
        let mut n = 0;
        while self.nth(n) == TokenKind::Identifier && MODIFIERS.contains(&self.word(n)) {
            n += 1;
        }
        let keyword = match self.nth(n) {
            TokenKind::Identifier => matches!(self.word(n), "class" | "interface"),
            TokenKind::Enum => n > 0,
            _ => false,
        };
        keyword && is_name(self.nth(n + 1))
    }

    /// The text of the token `n` places ahead.
    pub(super) fn word(&self, n: usize) -> &str {
        match self.tokens.get(self.position + n) {
            Some(token) => self.text(*token),
            None => "",
        }
    }

    /// A class declaration at the top of a file, which strict syntax rejects.
    pub(super) fn class_item(&mut self) -> Parse<Item> {
        // The class comes before the legacy syntax inside it.
        let index = self.legacy.len();
        let class = self.class()?;
        let message = "`class` is not allowed as an identifier because it is a Groovy keyword";
        let diagnostic = crate::Diagnostic::new(message, class.name.span);
        self.legacy.insert(index, diagnostic);
        Ok(Item::Class(Box::new(class)))
    }

    /// `modifiers class Name extends A implements B { members }`, or an enum.
    fn class(&mut self) -> Parse<Class> {
        let start = self.current().span;
        self.modifiers();
        let enumeration = self.at(TokenKind::Enum);
        if self.word(0) == "interface" {
            return Err(self.error_here("Groovy interfaces are not supported"));
        }
        self.bump();
        let name = self.name()?;
        // The classes a class extends or implements do not matter to the static members here.
        while matches!(self.word(0), "extends" | "implements") {
            self.bump();
            self.qualified_class_name()?;
            while self.eat(TokenKind::Comma) {
                self.qualified_class_name()?;
            }
        }
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "'{'")?;
        let outer = std::mem::replace(&mut self.brackets, 0);
        let groovy = std::mem::replace(&mut self.groovy, true);
        let body = self.class_body(&name, enumeration);
        self.brackets = outer;
        self.groovy = groovy;
        let (constants, members) = body?;
        self.expect(TokenKind::RightBrace, "'}' to close the class")?;
        Ok(Class {
            name,
            enumeration,
            constants,
            members,
            span: self.span_from(start),
        })
    }

    /// The constants of an enum, if any, then the members up to the closing brace.
    fn class_body(&mut self, class: &Name, enumeration: bool) -> Parse<(Vec<Name>, Vec<Member>)> {
        let mut constants = Vec::new();
        if enumeration {
            self.skip_newlines();
            while self.at_name() && !self.at_member_start() {
                constants.push(self.name()?);
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.eat(TokenKind::Semicolon);
        }
        let mut members = Vec::new();
        loop {
            self.skip_separators();
            if self.at(TokenKind::RightBrace) || self.at(TokenKind::EndOfFile) {
                return Ok((constants, members));
            }
            members.extend(self.member(class)?);
        }
    }

    /// Whether a member rather than an enum constant starts here.
    fn at_member_start(&self) -> bool {
        MODIFIERS.contains(&self.word(0)) || self.nth(1) == TokenKind::LeftParen
    }

    /// Skips modifiers and reports whether `static` was one of them.
    fn modifiers(&mut self) -> bool {
        let mut is_static = false;
        while self.at(TokenKind::Identifier) && MODIFIERS.contains(&self.word(0)) {
            is_static |= self.word(0) == "static";
            self.bump();
            self.skip_newlines();
        }
        is_static
    }

    /// A field, method, constructor or nested class; one declaration may hold several fields.
    fn member(&mut self, class: &Name) -> Parse<Vec<Member>> {
        if self.at_class() || self.at(TokenKind::Enum) {
            return Ok(vec![Member::Class(Box::new(self.class()?))]);
        }
        let start = self.current().span;
        let is_static = self.modifiers();
        // A constructor has the name of its class and no return type.
        if self.at_name() && self.word(0) == class.text && self.nth(1) == TokenKind::LeftParen {
            let name = self.name()?;
            let function = self.method_rest(start, name, None)?;
            return Ok(vec![function.into_member(false)]);
        }
        let def = self.eat(TokenKind::Def);
        let untyped = self.at_name()
            && (matches!(self.nth(1), TokenKind::LeftParen) || (def && self.at_field_end(1)));
        let annotation = if untyped {
            None
        } else if self.word(0) == "void" {
            self.bump();
            None
        } else {
            Some(self.legacy_type()?)
        };
        let name = self.name()?;
        if self.at(TokenKind::LeftParen) {
            let function = self.method_rest(start, name, annotation)?;
            return Ok(vec![function.into_member(is_static)]);
        }
        let mut fields = Vec::new();
        let mut name = name;
        loop {
            let value = if self.eat(TokenKind::Assign) {
                self.skip_newlines();
                Some(self.expression()?)
            } else {
                None
            };
            fields.push(Member::Field {
                name,
                is_static,
                annotation: annotation.clone(),
                value,
                span: self.span_from(start),
            });
            if !self.eat(TokenKind::Comma) {
                return Ok(fields);
            }
            self.skip_newlines();
            name = self.name()?;
        }
    }

    /// Whether the token `n` places ahead ends the name of a field.
    fn at_field_end(&self, n: usize) -> bool {
        matches!(
            self.nth(n),
            TokenKind::Assign
                | TokenKind::Newline
                | TokenKind::Semicolon
                | TokenKind::Comma
                | TokenKind::RightBrace
        )
    }

    /// The parameters, `throws` clause and body of a method, after its name.
    fn method_rest(
        &mut self,
        start: crate::Span,
        name: Name,
        return_type: Option<crate::ast::Type>,
    ) -> Parse<Method> {
        self.expect(TokenKind::LeftParen, "'('")?;
        self.skip_newlines();
        let mut parameters = Vec::new();
        let mut varargs = false;
        while !self.at(TokenKind::RightParen) {
            if varargs {
                return Err(self.error_here("Expected ')' after the variable arguments"));
            }
            let (parameter, rest) = self.method_parameter()?;
            parameters.push(parameter);
            varargs = rest;
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RightParen, "')'")?;
        if self.word(0) == "throws" {
            self.bump();
            self.qualified_class_name()?;
            while self.eat(TokenKind::Comma) {
                self.qualified_class_name()?;
            }
        }
        self.skip_newlines();
        let outer = std::mem::replace(&mut self.brackets, 0);
        let body = self.block();
        self.brackets = outer;
        Ok(Method {
            function: Function {
                name,
                parameters,
                return_type,
                body: body?,
                span: self.span_from(start),
            },
            varargs,
        })
    }

    /// A parameter like `x`, `def x`, `Map x`, `x = 1`, `String... names` or `... names`, and
    /// whether it takes the remaining arguments.
    fn method_parameter(&mut self) -> Parse<(Parameter, bool)> {
        self.modifiers();
        self.eat(TokenKind::Def);
        let untyped = (self.at_name() && !self.at_varargs(1) && self.at_parameter_end(1))
            || self.at_varargs(0);
        let annotation = if untyped {
            None
        } else {
            Some(self.legacy_type()?)
        };
        let varargs = self.at_varargs(0);
        if varargs {
            // `...` is the tokens `..` and `.`.
            self.bump();
            self.bump();
        }
        let name = self.variable_name()?;
        let default = if self.eat(TokenKind::Assign) {
            self.skip_newlines();
            Some(self.expression()?)
        } else {
            None
        };
        let parameter = Parameter {
            name,
            annotation,
            default,
        };
        Ok((parameter, varargs))
    }

    /// Whether `...` starts `n` places ahead.
    fn at_varargs(&self, n: usize) -> bool {
        self.nth(n) == TokenKind::RangeInclusive && self.nth(n + 1) == TokenKind::Dot
    }

    fn at_parameter_end(&self, n: usize) -> bool {
        matches!(
            self.nth(n),
            TokenKind::Comma | TokenKind::RightParen | TokenKind::Assign | TokenKind::Newline
        )
    }

    /// `import a.b.C`, `import a.b.*`, `import a.b.C as D` or `import static a.b.C.member`,
    /// which strict syntax rejects.
    pub(super) fn legacy_import(&mut self) -> Parse<Item> {
        let start = self.bump().span;
        let member = self.word(0) == "static" && is_name(self.nth(1));
        if member {
            self.bump();
        }
        let mut parts = vec![self.import_part()?];
        while self.eat(TokenKind::Dot) {
            if self.eat(TokenKind::Multiply) {
                parts.push(Name {
                    text: "*".into(),
                    span: self.span_from(start),
                });
                break;
            }
            parts.push(self.import_part()?);
        }
        let alias = if self.eat(TokenKind::As) {
            Some(self.name()?)
        } else {
            None
        };
        let span = self.span_from(start);
        let message = "Groovy `import` declarations are not supported -- use fully-qualified name inline instead";
        self.legacy_at(message, span);
        Ok(Item::Import {
            name: QualifiedName { parts },
            alias,
            member,
            span,
        })
    }

    /// A part of an imported name, which may be a keyword like `as` in a package name.
    fn import_part(&mut self) -> Parse<Name> {
        if self.at_name() || matches!(self.peek(), TokenKind::As | TokenKind::Def) {
            let token = self.bump();
            return Ok(Name {
                text: self.text(token).to_owned(),
                span: token.span,
            });
        }
        Err(self.error_here("Expected a name"))
    }
}

/// A method and whether its last parameter takes the remaining arguments.
struct Method {
    function: Function,
    varargs: bool,
}

impl Method {
    fn into_member(self, is_static: bool) -> Member {
        Member::Method {
            function: self.function,
            is_static,
            varargs: self.varargs,
        }
    }
}
