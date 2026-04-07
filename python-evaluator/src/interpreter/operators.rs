//! Binary operators, comparisons, membership and truth values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::rc::Rc;

use indexmap::IndexSet;

use super::Interpreter;
use crate::error::Error;
use crate::number::{Arithmetic, arithmetic, compare_numbers, is_number};
use crate::types;
use crate::value::{Args, Dict, Key, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Add,
    Sub,
    Mul,
    MatMul,
    TrueDiv,
    FloorDiv,
    Mod,
    Pow,
    LShift,
    RShift,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
}

impl Operator {
    pub fn from_ast(operator: ruff_python_ast::Operator) -> Operator {
        use ruff_python_ast::Operator as O;
        match operator {
            O::Add => Operator::Add,
            O::Sub => Operator::Sub,
            O::Mult => Operator::Mul,
            O::MatMult => Operator::MatMul,
            O::Div => Operator::TrueDiv,
            O::FloorDiv => Operator::FloorDiv,
            O::Mod => Operator::Mod,
            O::Pow => Operator::Pow,
            O::LShift => Operator::LShift,
            O::RShift => Operator::RShift,
            O::BitAnd => Operator::BitAnd,
            O::BitOr => Operator::BitOr,
            O::BitXor => Operator::BitXor,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Operator::Add => "+",
            Operator::Sub => "-",
            Operator::Mul => "*",
            Operator::MatMul => "@",
            Operator::TrueDiv => "/",
            Operator::FloorDiv => "//",
            Operator::Mod => "%",
            Operator::Pow => "** or pow()",
            Operator::LShift => "<<",
            Operator::RShift => ">>",
            Operator::BitAnd => "&",
            Operator::BitOr => "|",
            Operator::BitXor => "^",
            Operator::Eq => "==",
            Operator::NotEq => "!=",
            Operator::Lt => "<",
            Operator::LtE => "<=",
            Operator::Gt => ">",
            Operator::GtE => ">=",
        }
    }

    /// The special method names for the operator and its reflection.
    fn methods(self) -> (&'static str, &'static str) {
        match self {
            Operator::Add => ("__add__", "__radd__"),
            Operator::Sub => ("__sub__", "__rsub__"),
            Operator::Mul => ("__mul__", "__rmul__"),
            Operator::MatMul => ("__matmul__", "__rmatmul__"),
            Operator::TrueDiv => ("__truediv__", "__rtruediv__"),
            Operator::FloorDiv => ("__floordiv__", "__rfloordiv__"),
            Operator::Mod => ("__mod__", "__rmod__"),
            Operator::Pow => ("__pow__", "__rpow__"),
            Operator::LShift => ("__lshift__", "__rlshift__"),
            Operator::RShift => ("__rshift__", "__rrshift__"),
            Operator::BitAnd => ("__and__", "__rand__"),
            Operator::BitOr => ("__or__", "__ror__"),
            Operator::BitXor => ("__xor__", "__rxor__"),
            Operator::Eq => ("__eq__", "__eq__"),
            Operator::NotEq => ("__ne__", "__ne__"),
            Operator::Lt => ("__lt__", "__gt__"),
            Operator::LtE => ("__le__", "__ge__"),
            Operator::Gt => ("__gt__", "__lt__"),
            Operator::GtE => ("__ge__", "__le__"),
        }
    }

    fn arithmetic(self) -> Option<Arithmetic> {
        Some(match self {
            Operator::Add => Arithmetic::Add,
            Operator::Sub => Arithmetic::Sub,
            Operator::Mul => Arithmetic::Mul,
            Operator::TrueDiv => Arithmetic::TrueDiv,
            Operator::FloorDiv => Arithmetic::FloorDiv,
            Operator::Mod => Arithmetic::Mod,
            Operator::Pow => Arithmetic::Pow,
            Operator::LShift => Arithmetic::LShift,
            Operator::RShift => Arithmetic::RShift,
            Operator::BitAnd => Arithmetic::And,
            Operator::BitOr => Arithmetic::Or,
            Operator::BitXor => Arithmetic::Xor,
            _ => return None,
        })
    }
}

fn repeat<T: Clone>(items: &[T], count: &Value) -> Option<Vec<T>> {
    let count = count.as_int()?;
    let count = count.max(0) as usize;
    let mut repeated = Vec::with_capacity(items.len() * count);
    for _ in 0..count {
        repeated.extend_from_slice(items);
    }
    Some(repeated)
}

impl Interpreter {
    /// `a <op> b` for arithmetic, bit and comparison operators.
    pub fn binary(&mut self, operator: Operator, a: &Value, b: &Value) -> Result<Value, Error> {
        if let Some(result) = self.try_binary(operator, a, b)? {
            return Ok(result);
        }
        match operator {
            Operator::Eq => return Ok(Value::Bool(self.equals(a, b)?)),
            Operator::NotEq => return Ok(Value::Bool(!self.equals(a, b)?)),
            _ => {}
        }
        let verb = if matches!(
            operator,
            Operator::Lt | Operator::LtE | Operator::Gt | Operator::GtE
        ) {
            format!(
                "'{}' not supported between instances of '{}' and '{}'",
                operator.symbol(),
                types::type_name(a),
                types::type_name(b)
            )
        } else {
            format!(
                "unsupported operand type(s) for {}: '{}' and '{}'",
                operator.symbol(),
                types::type_name(a),
                types::type_name(b)
            )
        };
        Err(Error::type_error(verb))
    }

    fn try_binary(
        &mut self,
        operator: Operator,
        a: &Value,
        b: &Value,
    ) -> Result<Option<Value>, Error> {
        let (a, b) = (a.plain(), b.plain());
        if let Some(op) = operator.arithmetic()
            && is_number(a)
            && is_number(b)
        {
            // Bit operators on two booleans give a boolean.
            if let (Value::Bool(x), Value::Bool(y)) = (a, b) {
                match operator {
                    Operator::BitAnd => return Ok(Some(Value::Bool(*x & *y))),
                    Operator::BitOr => return Ok(Some(Value::Bool(*x | *y))),
                    Operator::BitXor => return Ok(Some(Value::Bool(*x ^ *y))),
                    _ => {}
                }
            }
            return arithmetic(op, a, b);
        }
        if matches!(
            operator,
            Operator::Lt | Operator::LtE | Operator::Gt | Operator::GtE
        ) {
            if let Some(ordering) = self.builtin_ordering(a, b)? {
                let result = match operator {
                    Operator::Lt => ordering == Ordering::Less,
                    Operator::LtE => ordering != Ordering::Greater,
                    Operator::Gt => ordering == Ordering::Greater,
                    _ => ordering != Ordering::Less,
                };
                return Ok(Some(Value::Bool(result)));
            }
            if let (Some(x), Some(y)) = (set_items(a), set_items(b)) {
                let subset = x.iter().all(|item| y.contains(item));
                let superset = y.iter().all(|item| x.contains(item));
                let result = match operator {
                    Operator::Lt => subset && x.len() < y.len(),
                    Operator::LtE => subset,
                    Operator::Gt => superset && x.len() > y.len(),
                    _ => superset,
                };
                return Ok(Some(Value::Bool(result)));
            }
        }
        let builtin = match (operator, a, b) {
            (Operator::Add, Value::Str(x), Value::Str(y)) => {
                let mut text = String::with_capacity(x.len() + y.len());
                text.push_str(x);
                text.push_str(y);
                Some(Value::str(text))
            }
            (Operator::Add, Value::List(x), Value::List(y)) => {
                let mut items = x.borrow().clone();
                items.extend(y.borrow().iter().cloned());
                Some(Value::list(items))
            }
            (Operator::Add, Value::Tuple(x), Value::Tuple(y)) => {
                let mut items = x.to_vec();
                items.extend(y.iter().cloned());
                Some(Value::tuple(items))
            }
            (Operator::Add, Value::Bytes(x), Value::Bytes(y)) => {
                let mut bytes = x.to_vec();
                bytes.extend_from_slice(y);
                Some(Value::Bytes(bytes.into()))
            }
            (Operator::Mul, Value::Str(text), count) | (Operator::Mul, count, Value::Str(text))
                if count.as_int().is_some() =>
            {
                Some(Value::str(
                    text.repeat(count.as_int().unwrap_or(0).max(0) as usize),
                ))
            }
            (Operator::Mul, Value::List(items), count)
            | (Operator::Mul, count, Value::List(items))
                if count.as_int().is_some() =>
            {
                let items = items.borrow().clone();
                repeat(&items, count).map(Value::list)
            }
            (Operator::Mul, Value::Tuple(items), count)
            | (Operator::Mul, count, Value::Tuple(items))
                if count.as_int().is_some() =>
            {
                repeat(items, count).map(Value::tuple)
            }
            (Operator::Mod, Value::Str(template), values) => Some(Value::str(
                crate::format::percent_format(self, template, values)?,
            )),
            (Operator::BitOr, Value::Dict(x), Value::Dict(y)) => {
                let mut dict: Dict = x.borrow().clone();
                for (key, value) in y.borrow().entries.iter() {
                    dict.entries.insert(key.clone(), value.clone());
                }
                Some(Value::dict(dict))
            }
            (Operator::Eq, _, _) | (Operator::NotEq, _, _) => None,
            _ => None,
        };
        if builtin.is_some() {
            return Ok(builtin);
        }
        if let (Some(x), Some(y)) = (set_items(a), set_items(b)) {
            let result: Option<IndexSet<Key>> = match operator {
                Operator::BitOr => Some(x.union(&y).cloned().collect()),
                Operator::BitAnd => Some(x.intersection(&y).cloned().collect()),
                Operator::Sub => Some(x.difference(&y).cloned().collect()),
                Operator::BitXor => Some(x.symmetric_difference(&y).cloned().collect()),
                _ => None,
            };
            if let Some(result) = result {
                return Ok(Some(match a {
                    Value::FrozenSet(_) => Value::FrozenSet(Rc::new(result)),
                    _ => Value::set(result),
                }));
            }
        }
        // Objects of Python classes and native objects.
        let (forward, reflected) = operator.methods();
        if let Value::Native(object) = a
            && let Some(result) = object.binary(self, operator, b, false)?
        {
            return Ok(Some(result));
        }
        if let Value::Instance(_) = a
            && let Some(method) = self.lookup_special(a, forward)
        {
            let result = self.call(&method, Args::new(vec![b.clone()]))?;
            if !matches!(result, Value::NotImplemented) {
                return Ok(Some(result));
            }
        }
        if let Value::Native(object) = b
            && let Some(result) = object.binary(self, operator, a, true)?
        {
            return Ok(Some(result));
        }
        if let Value::Instance(_) = b
            && let Some(method) = self.lookup_special(b, reflected)
        {
            let result = self.call(&method, Args::new(vec![a.clone()]))?;
            if !matches!(result, Value::NotImplemented) {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    /// `a == b`.
    pub fn equals(&mut self, a: &Value, b: &Value) -> Result<bool, Error> {
        let (a, b) = (a.plain(), b.plain());
        Ok(match (a, b) {
            (Value::Str(x), Value::Str(y)) => x == y,
            (x, y) if is_number(x) && is_number(y) => {
                compare_numbers(x, y) == Some(Ordering::Equal)
            }
            (Value::None, Value::None) => true,
            (Value::List(x), Value::List(y)) => {
                if Rc::ptr_eq(x, y) {
                    return Ok(true);
                }
                let (x, y) = (x.borrow().clone(), y.borrow().clone());
                self.all_equal(&x, &y)?
            }
            (Value::Tuple(x), Value::Tuple(y)) => self.all_equal(x, y)?,
            (Value::Dict(x), Value::Dict(y)) => {
                if Rc::ptr_eq(x, y) {
                    return Ok(true);
                }
                let x = x.borrow().entries.clone();
                let y = y.borrow().entries.clone();
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (key, value) in &x {
                    match y.get(key) {
                        Some(other) => {
                            if !self.equals(value, other)? {
                                return Ok(false);
                            }
                        }
                        None => return Ok(false),
                    }
                }
                true
            }
            (Value::Set(_) | Value::FrozenSet(_), Value::Set(_) | Value::FrozenSet(_)) => {
                let (x, y) = (
                    set_items(a).unwrap_or_default(),
                    set_items(b).unwrap_or_default(),
                );
                x.len() == y.len() && x.iter().all(|item| y.contains(item))
            }
            (Value::Bytes(x), Value::Bytes(y)) => x == y,
            (Value::Range(x), Value::Range(y)) => {
                x.len() == y.len()
                    && (x.is_empty() || (x.start == y.start && (x.len() == 1 || x.step == y.step)))
            }
            (Value::Instance(_), _) | (_, Value::Instance(_)) => {
                for (left, right) in [(a, b), (b, a)] {
                    if let Value::Instance(_) = left
                        && let Some(method) = self.lookup_special(left, "__eq__")
                    {
                        let result = self.call(&method, Args::new(vec![right.clone()]))?;
                        if !matches!(result, Value::NotImplemented) {
                            return self.truthy(&result);
                        }
                    }
                }
                a.is(b)
            }
            (Value::Native(object), other) | (other, Value::Native(object)) => {
                if let Some(result) = object.binary(self, Operator::Eq, other, false)? {
                    return self.truthy(&result);
                }
                crate::value::key_equal(a, b)
            }
            _ => crate::value::key_equal(a, b),
        })
    }

    fn all_equal(&mut self, x: &[Value], y: &[Value]) -> Result<bool, Error> {
        if x.len() != y.len() {
            return Ok(false);
        }
        for (a, b) in x.iter().zip(y) {
            if !self.equals(a, b)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The order of two values of builtin types, `None` when they have none.
    fn builtin_ordering(&mut self, a: &Value, b: &Value) -> Result<Option<Ordering>, Error> {
        Ok(match (a.plain(), b.plain()) {
            (x, y) if is_number(x) && is_number(y) => compare_numbers(x, y),
            (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
            (Value::Bytes(x), Value::Bytes(y)) => Some(x.cmp(y)),
            (Value::List(x), Value::List(y)) => {
                let (x, y) = (x.borrow().clone(), y.borrow().clone());
                Some(self.sequence_ordering(&x, &y)?)
            }
            (Value::Tuple(x), Value::Tuple(y)) => Some(self.sequence_ordering(x, y)?),
            _ => None,
        })
    }

    fn sequence_ordering(&mut self, x: &[Value], y: &[Value]) -> Result<Ordering, Error> {
        for (a, b) in x.iter().zip(y) {
            if self.equals(a, b)? {
                continue;
            }
            return self.compare(a, b);
        }
        Ok(x.len().cmp(&y.len()))
    }

    /// The order of two values for sorting; a `TypeError` when they have none.
    pub fn compare(&mut self, a: &Value, b: &Value) -> Result<Ordering, Error> {
        if let Some(ordering) = self.builtin_ordering(a, b)? {
            return Ok(ordering);
        }
        let less = self.binary(Operator::Lt, a, b)?;
        if self.truthy(&less)? {
            return Ok(Ordering::Less);
        }
        let greater = self.binary(Operator::Lt, b, a)?;
        if self.truthy(&greater)? {
            return Ok(Ordering::Greater);
        }
        Ok(Ordering::Equal)
    }
}
