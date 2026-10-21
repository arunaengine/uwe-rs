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
use crate::value::{Args, Dict, Instance, Key, Value};

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
            // Unions of types, like `str | None`.
            (Operator::BitOr, x, y)
                if crate::types::UnionType::is_member(x)
                    && crate::types::UnionType::is_member(y)
                    && !(x.is_none() && y.is_none()) =>
            {
                Some(crate::types::UnionType::of(x, y))
            }
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
            let name = match operator {
                Operator::BitOr => Some("union"),
                Operator::BitAnd => Some("intersection"),
                Operator::Sub => Some("difference"),
                Operator::BitXor => Some("symmetric_difference"),
                _ => None,
            };
            let result = match name {
                Some(name) => Some(self.combine(name, &x, &y)?),
                None => None,
            };
            let name = name.unwrap_or_default();
            if let Some(result) = result {
                let value = match a {
                    Value::FrozenSet(_) => Value::FrozenSet(Rc::new(result)),
                    _ => Value::set(result),
                };
                // The order of the result follows CPython's set code for these operands.
                let operands = (crate::set_table::Operand::of(a, &x))
                    .zip(crate::set_table::Operand::of(b, &y));
                if let Some((this, other)) = operands
                    && let Some(table) = crate::set_table::derived(name, &this, &[other])
                {
                    crate::hashing::register(&value, table);
                }
                return Ok(Some(value));
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

    /// `item in container`.
    pub fn contains(&mut self, container: &Value, item: &Value) -> Result<bool, Error> {
        let item = item.plain();
        if let Value::Class(class) = container.plain()
            && let Some(members) = crate::modules::enums::members(class)
        {
            return Ok(members
                .iter()
                .any(|member| crate::value::key_equal(member, item)));
        }
        match container.plain() {
            Value::Str(text) => match item {
                Value::Str(part) => Ok(text.contains(&**part)),
                other => Err(Error::type_error(format!(
                    "'in <string>' requires string as left operand, not {}",
                    types::type_name(other)
                ))),
            },
            Value::Bytes(bytes) => {
                match item {
                    Value::Bytes(part) => Ok(part.is_empty()
                        || bytes.windows(part.len()).any(|window| window == &part[..])),
                    Value::Int(byte) => Ok(bytes.contains(&(*byte as u8))),
                    _ => Err(Error::type_error("a bytes-like object is required")),
                }
            }
            Value::Dict(dict) => {
                let key = self.dict_key(dict, item)?;
                Ok(dict.borrow().entries.contains_key(&key))
            }
            Value::Set(set) => {
                let key = self.member_key(set, item)?;
                Ok(set.borrow().contains(&key))
            }
            Value::FrozenSet(set) => {
                let key = self.set_key(set, item)?;
                Ok(set.contains(&key))
            }
            Value::Range(range) => Ok(match item.as_int() {
                Some(value) => {
                    let inside = if range.step > 0 {
                        value >= range.start && value < range.stop
                    } else {
                        value <= range.start && value > range.stop
                    };
                    inside && (value - range.start) % range.step == 0
                }
                None => false,
            }),
            Value::List(items) => {
                let items = items.borrow().clone();
                self.any_equal(&items, item)
            }
            Value::Tuple(items) => {
                let items = items.clone();
                self.any_equal(&items, item)
            }
            Value::Native(object) => match object.contains(self, item)? {
                Some(found) => Ok(found),
                None => {
                    let items = self.iterate(container)?;
                    self.any_equal(&items, item)
                }
            },
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(container, "__contains__") {
                    let result = self.call(&method, Args::new(vec![item.clone()]))?;
                    return self.truthy(&result);
                }
                let items = self.iterate(container)?;
                self.any_equal(&items, item)
            }
            other => {
                let items = self.iterate(other).map_err(|_| {
                    Error::type_error(format!(
                        "argument of type '{}' is not iterable",
                        types::type_name(other)
                    ))
                })?;
                self.any_equal(&items, item)
            }
        }
    }

    fn any_equal(&mut self, items: &[Value], item: &Value) -> Result<bool, Error> {
        for candidate in items {
            if candidate.is(item) || self.equals(candidate, item)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `bool(value)`.
    pub fn truthy(&mut self, value: &Value) -> Result<bool, Error> {
        if value.as_native::<crate::pandas::Series>().is_some()
            || value.as_native::<crate::pandas::DataFrame>().is_some()
            || value.as_native::<crate::pandas::IndexObject>().is_some()
        {
            return Err(Error::value_error(format!(
                "The truth value of a {} is ambiguous",
                types::type_name(value)
            )));
        }
        Ok(match value.plain() {
            Value::None => false,
            Value::Bool(value) => *value,
            Value::Int(value) => *value != 0,
            Value::BigInt(_) => true,
            Value::Float(value) => *value != 0.0,
            Value::Str(text) => !text.is_empty(),
            Value::Bytes(bytes) => !bytes.is_empty(),
            Value::List(items) => !items.borrow().is_empty(),
            Value::Tuple(items) => !items.is_empty(),
            Value::Dict(dict) => !dict.borrow().entries.is_empty(),
            Value::Set(set) => !set.borrow().is_empty(),
            Value::FrozenSet(set) => !set.is_empty(),
            Value::Range(range) => !range.is_empty(),
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(value, "__bool__") {
                    let result = self.call(&method, Args::default())?;
                    return self.truthy(&result);
                }
                if let Some(method) = self.lookup_special(value, "__len__") {
                    let result = self.call(&method, Args::default())?;
                    return Ok(result.as_int().unwrap_or(0) != 0);
                }
                true
            }
            Value::Native(object) => match object.truthy() {
                Some(truth) => truth,
                None => object.length().is_none_or(|length| length > 0),
            },
            _ => true,
        })
    }

    /// `not value`, `-value`, `+value` and `~value`.
    pub fn unary(&mut self, operator: &str, value: &Value) -> Result<Value, Error> {
        let value = value.plain();
        match (operator, value) {
            ("not", _) => Ok(Value::Bool(!self.truthy(value)?)),
            ("-", Value::Bool(b)) => Ok(Value::Int(-i64::from(*b))),
            ("-", Value::Int(i)) => Ok(match i.checked_neg() {
                Some(negated) => Value::Int(negated),
                None => Value::from_bigint(-num_bigint::BigInt::from(*i)),
            }),
            ("-", Value::BigInt(i)) => Ok(Value::from_bigint(-(**i).clone())),
            ("-", Value::Float(f)) => Ok(Value::Float(-f)),
            ("+", Value::Bool(b)) => Ok(Value::Int(i64::from(*b))),
            ("+", Value::Int(_) | Value::BigInt(_) | Value::Float(_)) => Ok(value.clone()),
            ("~", Value::Bool(b)) => Ok(Value::Int(!i64::from(*b))),
            ("~", Value::Int(i)) => Ok(Value::Int(!i)),
            ("~", Value::BigInt(i)) => Ok(Value::from_bigint(-(**i).clone() - 1)),
            (_, Value::Native(object)) => match object.unary(self, operator)? {
                Some(result) => Ok(result),
                None => Err(Error::type_error(format!(
                    "bad operand type for unary {operator}: '{}'",
                    object.type_name()
                ))),
            },
            (_, Value::Instance(_)) => {
                let name = match operator {
                    "-" => "__neg__",
                    "+" => "__pos__",
                    _ => "__invert__",
                };
                match self.lookup_special(value, name) {
                    Some(method) => self.call(&method, Args::default()),
                    None => Err(Error::type_error(format!(
                        "bad operand type for unary {operator}: '{}'",
                        types::type_name(value)
                    ))),
                }
            }
            _ => Err(Error::type_error(format!(
                "bad operand type for unary {operator}: '{}'",
                types::type_name(value)
            ))),
        }
    }

    /// A dict key or set member for a value.
    pub fn key(&mut self, value: &Value) -> Result<Key, Error> {
        self.prepare_key(value.plain())?;
        Key::new(value.plain().clone())
    }

    /// The key of a dict for a value: a key that equals it by `__eq__` when the value is
    /// not there itself, as CPython's dict finds a key by its hash and then by `==`.
    pub fn dict_key(
        &mut self,
        dict: &std::cell::RefCell<Dict>,
        value: &Value,
    ) -> Result<Key, Error> {
        let key = self.key(value)?;
        if dict.borrow().entries.contains_key(&key) {
            return Ok(key);
        }
        let found = self.equal_key(&key, || dict.borrow().entries.keys().cloned().collect())?;
        Ok(found.unwrap_or(key))
    }

    /// The member of a mutable set for a value, found as `dict_key` finds a key, without a
    /// copy of the set.
    pub fn member_key(
        &mut self,
        set: &std::cell::RefCell<IndexSet<Key>>,
        value: &Value,
    ) -> Result<Key, Error> {
        let key = self.key(value)?;
        if set.borrow().contains(&key) {
            return Ok(key);
        }
        let found = self.equal_key(&key, || set.borrow().iter().cloned().collect())?;
        Ok(found.unwrap_or(key))
    }

    /// Adds a member to a set that is being made, unless a member of another kind equals it
    /// by `__eq__`: the set keeps the member it has, as CPython does.
    pub fn add_member(&mut self, members: &mut IndexSet<Key>, key: Key) -> Result<(), Error> {
        if members.contains(&key)
            || (self.equal_key(&key, || members.iter().cloned().collect())?).is_some()
        {
            return Ok(());
        }
        members.insert(key);
        Ok(())
    }

    /// Sets an entry of a dict that is being made; a key of another kind that equals the key
    /// by `__eq__` keeps its place and object, as CPython keeps the first key.
    pub fn set_entry(
        &mut self,
        entries: &mut crate::entries::Entries,
        key: Key,
        value: Value,
    ) -> Result<(), Error> {
        let found = match entries.contains_key(&key) {
            true => None,
            false => self.equal_key(&key, || entries.keys().cloned().collect())?,
        };
        entries.insert(found.unwrap_or(key), value);
        Ok(())
    }

    /// `union`, `intersection`, `difference` or `symmetric_difference` of two sets, where
    /// members of other kinds that are equal by `__eq__` count as one.
    pub fn combine(
        &mut self,
        name: &str,
        x: &IndexSet<Key>,
        y: &IndexSet<Key>,
    ) -> Result<IndexSet<Key>, Error> {
        let mut result = IndexSet::new();
        match name {
            "union" => {
                result = x.clone();
                for key in y {
                    self.add_member(&mut result, key.clone())?;
                }
            }
            // CPython's `set_intersection` walks the smaller set and keeps its members.
            "intersection" => {
                let (walked, other) = if y.len() <= x.len() { (y, x) } else { (x, y) };
                for key in walked {
                    let found = other.contains(key)
                        || (self.equal_key(key, || other.iter().cloned().collect())?).is_some();
                    if found {
                        result.insert(key.clone());
                    }
                }
            }
            "difference" => {
                for key in x {
                    let found = y.contains(key)
                        || (self.equal_key(key, || y.iter().cloned().collect())?).is_some();
                    if !found {
                        result.insert(key.clone());
                    }
                }
            }
            _ => {
                result = self.combine("difference", x, y)?;
                for key in self.combine("difference", y, x)? {
                    result.insert(key);
                }
            }
        }
        Ok(result)
    }

    /// The member of a set for a value, found as `dict_key` finds a key.
    pub fn set_key(&mut self, members: &IndexSet<Key>, value: &Value) -> Result<Key, Error> {
        let key = self.key(value)?;
        if members.contains(&key) {
            return Ok(key);
        }
        let found = self.equal_key(&key, || members.iter().cloned().collect())?;
        Ok(found.unwrap_or(key))
    }

    /// Among `candidates`, a key of another kind with the same hash that equals `key` by
    /// `__eq__`: an object of a class with `__eq__` and a text, number or tuple. Keys of one
    /// kind compare without Python code, so only these pairs need it.
    fn equal_key(
        &mut self,
        key: &Key,
        candidates: impl FnOnce() -> Vec<Key>,
    ) -> Result<Option<Key>, Error> {
        let instance = match key.0.plain() {
            Value::Instance(instance) => Some(instance),
            _ => None,
        };
        // Without objects of classes with `__hash__`, keys of one kind never meet another.
        if instance.is_none() && self.hashed.is_empty() {
            return Ok(None);
        }
        let hash = crate::hashing::python_hash(&key.0);
        let applies = match instance {
            Some(instance) => {
                instance.hashed.get().is_some()
                    && (instance.class.mro.borrow().iter())
                        .any(|class| class.dict.borrow().contains_key("__eq__"))
            }
            None => self.hashed.contains_key(&hash),
        };
        if !applies {
            return Ok(None);
        }
        for candidate in candidates() {
            let other = matches!(candidate.0.plain(), Value::Instance(_));
            if other == instance.is_some() || crate::hashing::python_hash(&candidate.0) != hash {
                continue;
            }
            if self.equals(&key.0, &candidate.0)? {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Gives the objects in a key whose classes define `__hash__` their hash and equality.
    fn prepare_key(&mut self, value: &Value) -> Result<(), Error> {
        match value {
            Value::Tuple(items) => items
                .iter()
                .try_for_each(|item| self.prepare_key(item.plain())),
            Value::Instance(instance) if instance.hashed.get().is_none() => {
                self.hash_instance(instance)
            }
            _ => Ok(()),
        }
    }

    /// Calls `__hash__` once and finds an equal object among earlier keys with that hash, so
    /// that keys compare without running Python code. The objects of a class that defines
    /// `__eq__` without `__hash__` are unhashable, as in Python.
    fn hash_instance(&mut self, instance: &Rc<Instance>) -> Result<(), Error> {
        let mut method = None;
        let mut equality = false;
        for class in instance.class.mro.borrow().iter() {
            let dict = class.dict.borrow();
            let defines_eq = dict.contains_key("__eq__");
            if method.is_none() {
                match dict.get("__hash__") {
                    Some(Value::None) => return Err(unhashable(&instance.class.name)),
                    Some(found) => method = Some(found.clone()),
                    None if defines_eq => return Err(unhashable(&instance.class.name)),
                    None => {}
                }
            }
            equality |= defines_eq;
        }
        let Some(method) = method else {
            return Ok(());
        };
        let object = Value::Instance(instance.clone());
        let result = self.call(&super::bind_method(&object, method), Args::default())?;
        let hash = match result.plain() {
            Value::Bool(_) | Value::Int(_) | Value::BigInt(_) => {
                crate::hashing::stable_hash(&result).unwrap_or_default()
            }
            _ => {
                return Err(Error::type_error(
                    "__hash__ method should return an integer",
                ));
            }
        };
        let mut class = None;
        if equality {
            let earlier: Vec<(Rc<Instance>, u64)> = (self.hashed.get(&hash).into_iter())
                .flatten()
                .filter_map(|(weak, class)| Some((weak.upgrade()?, *class)))
                .collect();
            for (other, other_class) in earlier {
                if self.equals(&object, &Value::Instance(other))? {
                    class = Some(other_class);
                    break;
                }
            }
        }
        let class = class.unwrap_or_else(|| {
            self.equality_classes += 1;
            let entries = self.hashed.entry(hash).or_default();
            entries.retain(|(weak, _)| weak.strong_count() > 0);
            entries.push((Rc::downgrade(instance), self.equality_classes));
            self.equality_classes
        });
        instance.hashed.set(Some((hash, class)));
        Ok(())
    }
}

fn unhashable(name: &str) -> Error {
    Error::type_error(format!("unhashable type: '{name}'"))
}

/// The members of a set or frozenset.
fn set_items(value: &Value) -> Option<IndexSet<Key>> {
    match value.plain() {
        Value::Set(set) => Some(set.borrow().clone()),
        Value::FrozenSet(set) => Some((**set).clone()),
        _ => None,
    }
}
