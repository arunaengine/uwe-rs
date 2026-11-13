//! Lowers legacy Groovy classes, fields, methods, constructors, and nested enums.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_parser::Span;
use nextflow_parser::ast::{self, Member};

use super::Lowering;
use crate::lowered::{
    ClassDef, Code, Expression, ExpressionKind as Kind, FieldDef, MethodDef, Parameter,
};

/// What code inside a class can name without qualification.
#[derive(Clone, Debug, Default)]
pub(super) struct ClassContext {
    /// The name of the class, after the classes it is nested in.
    pub name: Rc<str>,
    /// The fields of the class and the classes it is nested in, with the class that has them.
    pub static_fields: HashMap<Rc<str>, Rc<str>>,
    pub instance_fields: Vec<Rc<str>>,
    /// The methods of the class and the classes it is nested in, with the class that has them.
    pub static_methods: HashMap<Rc<str>, Rc<str>>,
    pub instance_methods: Vec<Rc<str>>,
    /// The nested classes that the class can name, by simple name.
    pub nested: HashMap<Rc<str>, Rc<str>>,
    /// Whether the code is static: a static method or the value of a static field.
    pub is_static: bool,
}

impl Lowering<'_> {
    /// Lowers a class and the classes nested in it into `classes`, outermost first.
    pub(super) fn class(
        &mut self,
        class: &ast::Class,
        outer: Option<&ClassContext>,
        classes: &mut Vec<Rc<ClassDef>>,
    ) {
        let name: Rc<str> = match outer {
            Some(outer) => format!("{}.{}", outer.name, class.name.text).into(),
            None => class.name.text.as_str().into(),
        };
        let mut context = outer.cloned().unwrap_or_default();
        context.name = name.clone();
        context.instance_fields.clear();
        context.instance_methods.clear();
        let mut nested = Vec::new();
        for member in &class.members {
            match member {
                Member::Field {
                    name: field,
                    is_static: true,
                    ..
                } => {
                    context
                        .static_fields
                        .insert(field.text.as_str().into(), name.clone());
                }
                Member::Field { name: field, .. } => {
                    context.instance_fields.push(field.text.as_str().into());
                }
                Member::Method {
                    function,
                    is_static,
                    ..
                } if *is_static => {
                    context
                        .static_methods
                        .insert(function.name.text.as_str().into(), name.clone());
                }
                Member::Method { function, .. } => {
                    context
                        .instance_methods
                        .push(function.name.text.as_str().into());
                }
                Member::Class(inner) => {
                    let qualified: Rc<str> = format!("{name}.{}", inner.name.text).into();
                    context
                        .nested
                        .insert(inner.name.text.as_str().into(), qualified.clone());
                    nested.push(qualified);
                }
            }
        }
        // The constants of an enum are its static fields too.
        for constant in &class.constants {
            context
                .static_fields
                .insert(constant.text.as_str().into(), name.clone());
        }
        let previous = self.class.replace(context.clone());
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut constructors = Vec::new();
        for member in &class.members {
            match member {
                Member::Field {
                    name: field,
                    is_static,
                    annotation,
                    value,
                    span,
                } => {
                    self.set_static(*is_static);
                    let value = value
                        .as_ref()
                        .map(|value| Rc::new(self.field_value(field, value, *is_static, *span)));
                    fields.push(FieldDef {
                        name: field.text.as_str().into(),
                        is_static: *is_static,
                        class: annotation.as_ref().and_then(simple_type),
                        value,
                    });
                }
                Member::Method {
                    function,
                    is_static,
                    varargs,
                } => {
                    let constructor = function.name.text == class.name.text
                        && function.return_type.is_none()
                        && !is_static;
                    self.set_static(*is_static);
                    let method = self.method(function, *is_static, *varargs);
                    if constructor {
                        constructors.push(method);
                    } else {
                        methods.push(method);
                    }
                }
                Member::Class(_) => {}
            }
        }
        self.class = previous;
        let def = ClassDef {
            name: name.clone(),
            enumeration: class.enumeration,
            constants: class
                .constants
                .iter()
                .map(|constant| constant.text.as_str().into())
                .collect(),
            fields,
            methods,
            constructors,
            nested,
            span: class.span,
        };
        classes.push(Rc::new(def));
        for member in &class.members {
            if let Member::Class(inner) = member {
                self.class(inner, Some(&context), classes);
            }
        }
    }

    fn set_static(&mut self, is_static: bool) {
        if let Some(context) = &mut self.class {
            context.is_static = is_static;
        }
    }

    /// The initial value of a field, as code; an instance field takes the object, `this`.
    fn field_value(
        &mut self,
        field: &ast::Name,
        value: &ast::Expression,
        is_static: bool,
        span: Span,
    ) -> Code {
        self.scopes.push_frame(false);
        let parameters = if is_static {
            Vec::new()
        } else {
            vec![self.this_parameter(span)]
        };
        let value = self.expression(value);
        let layout = self.scopes.pop_frame();
        Code {
            name: field.text.as_str().into(),
            parameters,
            returns: None,
            implicit_parameter: false,
            slot_count: layout.slot_count,
            captures: layout.captures,
            capture_names: layout.capture_names,
            module_reads: layout.module_reads,
            body: vec![crate::lowered::Statement {
                span: value.span,
                kind: crate::lowered::StatementKind::Expression(value),
            }],
            span,
        }
    }

    /// A method or constructor; one of the objects takes the object, `this`, first.
    fn method(&mut self, function: &ast::Function, is_static: bool, varargs: bool) -> MethodDef {
        self.scopes.push_frame(false);
        let mut parameters = Vec::new();
        if !is_static {
            parameters.push(self.this_parameter(function.span));
        }
        for parameter in &function.parameters {
            let default = parameter
                .default
                .as_ref()
                .map(|value| self.expression(value));
            let slot = self.declare_typed(&parameter.name, parameter.annotation.as_ref());
            parameters.push(Parameter {
                name: parameter.name.text.as_str().into(),
                class: parameter.annotation.as_ref().and_then(simple_type),
                slot,
                default,
                varargs: super::is_array(parameter),
            });
        }
        let body = self.statements(&function.body.statements);
        let layout = self.scopes.pop_frame();
        let code = Code {
            name: function.name.text.as_str().into(),
            parameters,
            returns: None,
            implicit_parameter: false,
            slot_count: layout.slot_count,
            captures: layout.captures,
            capture_names: layout.capture_names,
            module_reads: layout.module_reads,
            body,
            span: function.span,
        };
        MethodDef {
            name: function.name.text.as_str().into(),
            is_static,
            varargs,
            types: function
                .parameters
                .iter()
                .map(|parameter| parameter.annotation.as_ref().and_then(simple_type))
                .collect(),
            returns: function.return_type.as_ref().and_then(simple_type),
            code: Rc::new(code),
        }
    }

    fn this_parameter(&mut self, span: Span) -> Parameter {
        let name = ast::Name {
            text: "this".into(),
            span,
        };
        Parameter {
            name: "this".into(),
            class: None,
            slot: self.declare(&name),
            default: None,
            varargs: false,
        }
    }

    /// A name of a class, an unqualified member of the class being lowered, or `None`.
    pub(super) fn class_member(&mut self, name: &str, span: Span) -> Option<Kind> {
        let class = |name: &str| {
            Box::new(Expression {
                kind: Kind::LibraryClass(name.into()),
                span,
            })
        };
        let context = self.class.as_ref()?;
        if name == "this" && context.is_static {
            return Some(Kind::LibraryClass(context.name.clone()));
        }
        if let Some(owner) = context.static_fields.get(name).cloned() {
            return Some(Kind::Property {
                target: class(&owner),
                name: name.into(),
                access: ast::Access::Normal,
            });
        }
        if context.instance_fields.iter().any(|field| &**field == name) && !context.is_static {
            let this = self.scopes.resolve("this")?;
            return Some(Kind::Property {
                target: Box::new(Expression {
                    kind: Kind::Variable(this),
                    span,
                }),
                name: name.into(),
                access: ast::Access::Normal,
            });
        }
        context
            .nested
            .get(name)
            .map(|nested| Kind::LibraryClass(nested.clone()))
    }

    /// A call of a method of the class being lowered, without a receiver, or `None`.
    pub(super) fn class_call(
        &mut self,
        name: &str,
        span: Span,
        arguments: crate::lowered::Arguments,
    ) -> Result<Kind, crate::lowered::Arguments> {
        let Some(context) = self.class.as_ref() else {
            return Err(arguments);
        };
        let instance = context
            .instance_methods
            .iter()
            .any(|method| &**method == name);
        let target = if instance && !context.is_static {
            match self.scopes.resolve("this") {
                Some(this) => Kind::Variable(this),
                None => return Err(arguments),
            }
        } else if let Some(owner) = context.static_methods.get(name) {
            Kind::LibraryClass(owner.clone())
        } else {
            return Err(arguments);
        };
        Ok(Kind::CallMethod {
            target: Box::new(Expression { kind: target, span }),
            name: name.into(),
            access: ast::Access::Normal,
            arguments,
        })
    }
}

/// The simple name of a type, without package and type arguments; `None` for `Object`,
/// which takes any value.
pub fn simple_type(annotation: &ast::Type) -> Option<Rc<str>> {
    let name = annotation.name.parts.last()?;
    let dimensions = "[]".repeat(annotation.dimensions as usize);
    (name.text != "Object" || !dimensions.is_empty())
        .then(|| format!("{}{dimensions}", name.text).into())
}
