//! Evaluation of expressions, and assignment to targets.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, Comprehension, ConversionFlag, Expr, InterpolatedStringElement,
    Number, UnaryOp,
};
use ruff_text_size::Ranged;

use super::{Frame, FrameKind, Interpreter, Operator};
use crate::error::Error;
use crate::value::{Args, Code, Declared, Dict, Function, Key, Scope, Value};

impl Interpreter {
    /// Evaluates an expression in a frame; errors get the expression's line.
    pub fn eval(&mut self, frame: &Frame, expr: &Expr) -> Result<Value, Error> {
        self.eval_inner(frame, expr)
            .map_err(|error| error.with_location(frame.source.location(expr.start())))
    }

    fn eval_inner(&mut self, frame: &Frame, expr: &Expr) -> Result<Value, Error> {
        Ok(match expr {
            Expr::NumberLiteral(literal) => match &literal.value {
                Number::Int(int) => match int.as_i64() {
                    Some(value) => Value::Int(value),
                    None => crate::number::parse_int(&int.to_string(), 0)?,
                },
                Number::Float(value) => Value::Float(*value),
                Number::Complex { .. } => return Err(Error::unsupported("complex numbers")),
            },
            Expr::StringLiteral(literal) => Value::str(literal.value.to_str()),
            Expr::BytesLiteral(literal) => Value::Bytes(literal.value.bytes().collect()),
            Expr::BooleanLiteral(literal) => Value::Bool(literal.value),
            Expr::NoneLiteral(_) => Value::None,
            Expr::EllipsisLiteral(_) => Value::Ellipsis,
            Expr::FString(fstring) => Value::str(self.fstring(frame, fstring)?),
            Expr::Name(name) => self.lookup(frame, name.id.as_str())?,
            Expr::Attribute(attribute) => {
                let value = self.eval(frame, &attribute.value)?;
                self.get_attribute(&value, attribute.attr.as_str())?
            }
            Expr::Subscript(subscript) => {
                let value = self.eval(frame, &subscript.value)?;
                let key = self.eval(frame, &subscript.slice)?;
                self.get_item(&value, &key)?
            }
            Expr::Slice(slice) => {
                let part = |this: &mut Self, part: &Option<Box<Expr>>| -> Result<Value, Error> {
                    match part {
                        Some(expr) => this.eval(frame, expr),
                        None => Ok(Value::None),
                    }
                };
                let lower = part(self, &slice.lower)?;
                let upper = part(self, &slice.upper)?;
                let step = part(self, &slice.step)?;
                Value::Slice(Rc::new([lower, upper, step]))
            }
            Expr::Call(call) => self.eval_call(frame, call)?,
            Expr::BinOp(binary) => {
                let left = self.eval(frame, &binary.left)?;
                let right = self.eval(frame, &binary.right)?;
                self.binary(Operator::from_ast(binary.op), &left, &right)?
            }
            Expr::UnaryOp(unary) => {
                let operand = self.eval(frame, &unary.operand)?;
                let operator = match unary.op {
                    UnaryOp::Not => "not",
                    UnaryOp::USub => "-",
                    UnaryOp::UAdd => "+",
                    UnaryOp::Invert => "~",
                };
                self.unary(operator, &operand)?
            }
            Expr::BoolOp(operation) => {
                let mut result = Value::None;
                for (index, value) in operation.values.iter().enumerate() {
                    result = self.eval(frame, value)?;
                    if index + 1 == operation.values.len() {
                        break;
                    }
                    let truth = self.truthy(&result)?;
                    match operation.op {
                        BoolOp::And if !truth => break,
                        BoolOp::Or if truth => break,
                        _ => {}
                    }
                }
                result
            }
            Expr::Compare(compare) => {
                let mut left = self.eval(frame, &compare.operands[0])?;
                for (operator, right) in compare.ops.iter().zip(compare.operands[1..].iter()) {
                    let right = self.eval(frame, right)?;
                    if !self.compare_op(*operator, &left, &right)? {
                        return Ok(Value::Bool(false));
                    }
                    left = right;
                }
                Value::Bool(true)
            }
            Expr::If(conditional) => {
                let test = self.eval(frame, &conditional.test)?;
                if self.truthy(&test)? {
                    self.eval(frame, &conditional.body)?
                } else {
                    self.eval(frame, &conditional.orelse)?
                }
            }
            Expr::List(list) => Value::list(self.eval_elements(frame, &list.elts)?),
            Expr::Tuple(tuple) => Value::tuple(self.eval_elements(frame, &tuple.elts)?),
            Expr::Set(set) => {
                let mut items = IndexSet::new();
                for item in self.eval_elements(frame, &set.elts)? {
                    items.insert(Key::new(item)?);
                }
                Value::set(items)
            }
            Expr::Dict(dict) => {
                let mut result = Dict::default();
                for item in &dict.items {
                    match &item.key {
                        Some(key) => {
                            let key = self.eval(frame, key)?;
                            let value = self.eval(frame, &item.value)?;
                            result.entries.insert(Key::new(key.plain().clone())?, value);
                        }
                        None => {
                            let mapping = self.eval(frame, &item.value)?;
                            for (key, value) in self.mapping_items(&mapping)? {
                                result.entries.insert(Key::new(key)?, value);
                            }
                        }
                    }
                }
                Value::dict(result)
            }
            Expr::ListComp(comprehension) => {
                let mut items = Vec::new();
                self.comprehension(frame, &comprehension.generators, &mut |this, inner| {
                    items.push(this.eval(inner, &comprehension.elt)?);
                    Ok(())
                })?;
                Value::list(items)
            }
            Expr::Generator(generator) => {
                let mut items = Vec::new();
                self.comprehension(frame, &generator.generators, &mut |this, inner| {
                    items.push(this.eval(inner, &generator.elt)?);
                    Ok(())
                })?;
                Value::iterator(items)
            }
            Expr::SetComp(comprehension) => {
                let mut items = IndexSet::new();
                self.comprehension(frame, &comprehension.generators, &mut |this, inner| {
                    let item = this.eval(inner, &comprehension.elt)?;
                    items.insert(Key::new(item.plain().clone())?);
                    Ok(())
                })?;
                Value::set(items)
            }
            Expr::DictComp(comprehension) => {
                let mut result = Dict::default();
                self.comprehension(frame, &comprehension.generators, &mut |this, inner| {
                    match &comprehension.key {
                        Some(key) => {
                            let key = this.eval(inner, key)?;
                            let value = this.eval(inner, &comprehension.value)?;
                            result.entries.insert(Key::new(key.plain().clone())?, value);
                        }
                        None => {
                            let mapping = this.eval(inner, &comprehension.value)?;
                            for (key, value) in this.mapping_items(&mapping)? {
                                result.entries.insert(Key::new(key)?, value);
                            }
                        }
                    }
                    Ok(())
                })?;
                Value::dict(result)
            }
            Expr::Lambda(lambda) => {
                let defaults = self.eval_defaults(frame, lambda.parameters.as_deref())?;
                Value::Function(Rc::new(Function {
                    name: "<lambda>".into(),
                    qualname: "<lambda>".into(),
                    code: Code::Lambda(Rc::new(lambda.clone())),
                    defaults,
                    globals: frame.globals.clone(),
                    closure: frame.closure(),
                    source: frame.source.clone(),
                    generator: false,
                    declared: Rc::new(Declared::default()),
                    attributes: RefCell::new(IndexMap::new()),
                }))
            }
            Expr::Named(named) => {
                let value = self.eval(frame, &named.value)?;
                self.assign(frame, &named.target, value.clone())?;
                value
            }
            Expr::Starred(_) => {
                return Err(Error::new(
                    "SyntaxError",
                    "can't use starred expression here",
                ));
            }
            Expr::Yield(expression) => {
                let value = match &expression.value {
                    Some(value) => self.eval(frame, value)?,
                    None => Value::None,
                };
                self.push_yield(frame, value)?;
                Value::None
            }
            Expr::YieldFrom(expression) => {
                let value = self.eval(frame, &expression.value)?;
                for item in self.iterate(&value)? {
                    self.push_yield(frame, item)?;
                }
                Value::None
            }
            Expr::Await(_) => return Err(Error::unsupported("await")),
            Expr::TString(_) => return Err(Error::unsupported("template strings")),
            Expr::IpyEscapeCommand(_) => return Err(Error::unsupported("IPython commands")),
        })
    }

    fn push_yield(&mut self, frame: &Frame, value: Value) -> Result<(), Error> {
        match &frame.yields {
            Some(yields) => {
                let mut yields = yields.borrow_mut();
                if yields.len() > 10_000_000 {
                    return Err(Error::unsupported("a generator that does not end"));
                }
                yields.push(value);
                Ok(())
            }
            None => Err(Error::new("SyntaxError", "'yield' outside function")),
        }
    }

    fn compare_op(&mut self, operator: CmpOp, left: &Value, right: &Value) -> Result<bool, Error> {
        Ok(match operator {
            CmpOp::Eq => {
                let result = self.binary(Operator::Eq, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::NotEq => {
                let result = self.binary(Operator::NotEq, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::Lt => {
                let result = self.binary(Operator::Lt, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::LtE => {
                let result = self.binary(Operator::LtE, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::Gt => {
                let result = self.binary(Operator::Gt, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::GtE => {
                let result = self.binary(Operator::GtE, left, right)?;
                self.truthy(&result)?
            }
            CmpOp::Is => left.is(right),
            CmpOp::IsNot => !left.is(right),
            CmpOp::In => self.contains(right, left)?,
            CmpOp::NotIn => !self.contains(right, left)?,
        })
    }

    /// Evaluates list or tuple elements, spreading starred ones.
    fn eval_elements(&mut self, frame: &Frame, elements: &[Expr]) -> Result<Vec<Value>, Error> {
        let mut items = Vec::with_capacity(elements.len());
        for element in elements {
            if let Expr::Starred(starred) = element {
                let value = self.eval(frame, &starred.value)?;
                items.extend(self.iterate(&value)?);
            } else {
                items.push(self.eval(frame, element)?);
            }
        }
        Ok(items)
    }

    /// The key and value pairs of a mapping, for `**` and `dict()`.
    pub fn mapping_items(&mut self, mapping: &Value) -> Result<Vec<(Value, Value)>, Error> {
        match mapping.plain() {
            Value::Dict(dict) => Ok(dict
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.clone(), value.clone()))
                .collect()),
            other => {
                let keys = match self.find_attribute(other, "keys")? {
                    Some(keys) => {
                        let keys = self.call(&keys, Args::default())?;
                        self.iterate(&keys)?
                    }
                    None => {
                        return Err(Error::type_error(format!(
                            "'{}' object is not a mapping",
                            crate::types::type_name(other)
                        )));
                    }
                };
                let mut items = Vec::new();
                for key in keys {
                    let value = self.get_item(other, &key)?;
                    items.push((key, value));
                }
                Ok(items)
            }
        }
    }

    /// Runs the loops of a comprehension in a scope of its own, calling `body` for each item.
    fn comprehension(
        &mut self,
        frame: &Frame,
        generators: &[Comprehension],
        body: &mut dyn FnMut(&mut Self, &Frame) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // The first iterable is evaluated in the enclosing scope, as in Python.
        let Some(first) = generators.first() else {
            return body(self, frame);
        };
        let iterable = self.eval(frame, &first.iter)?;
        let parent = match frame.kind {
            FrameKind::Module => None,
            _ => Some(frame.locals.clone()),
        };
        let inner = Frame {
            locals: Scope::new(parent),
            globals: frame.globals.clone(),
            source: frame.source.clone(),
            kind: FrameKind::Comprehension,
            declared: Rc::new(Declared::default()),
            yields: None,
            method: frame.method.clone(),
        };
        self.comprehension_loop(&inner, generators, 0, Some(iterable), body)
    }

    fn comprehension_loop(
        &mut self,
        frame: &Frame,
        generators: &[Comprehension],
        level: usize,
        iterable: Option<Value>,
        body: &mut dyn FnMut(&mut Self, &Frame) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let Some(generator) = generators.get(level) else {
            return body(self, frame);
        };
        let iterable = match iterable {
            Some(iterable) => iterable,
            None => self.eval(frame, &generator.iter)?,
        };
        let items = self.iterate(&iterable)?;
        'items: for item in items {
            self.count_step()?;
            self.assign(frame, &generator.target, item)?;
            for condition in &generator.ifs {
                let test = self.eval(frame, condition)?;
                if !self.truthy(&test)? {
                    continue 'items;
                }
            }
            self.comprehension_loop(frame, generators, level + 1, None, body)?;
        }
        Ok(())
    }

    /// The text of an f-string.
    fn fstring(&mut self, frame: &Frame, fstring: &ast::ExprFString) -> Result<String, Error> {
        let mut text = String::new();
        for part in fstring.value.iter() {
            match part {
                ast::FStringPartRef::Literal(literal) => text.push_str(&literal.value),
                ast::FStringPartRef::FString(inner) => {
                    self.interpolated(frame, &inner.elements, &mut text)?;
                }
            }
        }
        Ok(text)
    }

    fn interpolated(
        &mut self,
        frame: &Frame,
        elements: &ast::InterpolatedStringElements,
        text: &mut String,
    ) -> Result<(), Error> {
        for element in elements.iter() {
            match element {
                InterpolatedStringElement::Literal(literal) => text.push_str(&literal.value),
                InterpolatedStringElement::Interpolation(interpolation) => {
                    let value = self.eval(frame, &interpolation.expression)?;
                    if let Some(debug) = &interpolation.debug_text {
                        text.push_str(debug.as_str());
                    }
                    let conversion = match interpolation.conversion {
                        ConversionFlag::None
                            if interpolation.debug_text.is_some()
                                && interpolation.format_spec.is_none() =>
                        {
                            Some('r')
                        }
                        ConversionFlag::None => None,
                        ConversionFlag::Str => Some('s'),
                        ConversionFlag::Repr => Some('r'),
                        ConversionFlag::Ascii => Some('a'),
                    };
                    let value = crate::format::convert(self, value, conversion)?;
                    let spec = match &interpolation.format_spec {
                        Some(spec) => {
                            let mut spec_text = String::new();
                            self.interpolated(frame, &spec.elements, &mut spec_text)?;
                            spec_text
                        }
                        None => String::new(),
                    };
                    text.push_str(&crate::format::format_value(self, &value, &spec)?);
                }
            }
        }
        Ok(())
    }
}
