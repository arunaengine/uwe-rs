//! Evaluates WDL expressions, strings with placeholders and command sections.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use indexmap::IndexMap;
use wdl_ast::v1::{
    CommandPart, CommandSection, Expr, LiteralExpr, LiteralString, Placeholder, PlaceholderOption,
    StringPart, StrippedCommandPart, StrippedStringPart,
};
use wdl_ast::AstToken;

use crate::library;
use crate::value::Value;

/// Why an expression has no value.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The expression fails, like an index out of bounds.
    Failed(String),
    /// The expression uses a feature that uwe does not support yet.
    Unsupported(String),
    /// The value depends on files that a task has not made yet.
    Unknown,
}

impl Error {
    pub fn failed(message: impl Into<String>) -> Self {
        Error::Failed(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Failed(message) | Error::Unsupported(message) => f.write_str(message),
            Error::Unknown => f.write_str("the value needs files that no task made yet"),
        }
    }
}

/// Which standard output stream `stdout()` or `stderr()` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What an expression reaches while it is evaluated: names, and files for the standard
/// library. The defaults fail, for places where WDL has no files to read.
pub trait Environment {
    fn lookup(&self, name: &str) -> Option<Value>;

    /// The text of a file, by its value.
    fn read(&mut self, file: &str) -> Result<String, Error> {
        Err(Error::failed(format!("cannot read {file} here")))
    }

    /// The files and directories that match a pattern in the work directory of a task.
    fn glob(&mut self, _pattern: &str) -> Result<Vec<String>, Error> {
        Err(Error::failed(
            "glob() is only available in the outputs of a task",
        ))
    }

    /// The file of a standard stream of a task.
    fn stream(&mut self, _stream: Stream) -> Result<Value, Error> {
        Err(Error::failed(
            "stdout() and stderr() are only available in the outputs of a task",
        ))
    }

    /// A new file with the content, as `write_lines` and the other `write_` functions make it.
    fn write(&mut self, _content: String) -> Result<Value, Error> {
        Err(Error::Unsupported(
            "the write_ functions are not supported here".into(),
        ))
    }

    /// The size of a file or directory in bytes.
    fn size(&mut self, file: &str) -> Result<u64, Error> {
        Err(Error::failed(format!(
            "cannot read the size of {file} here"
        )))
    }
}

/// The value of an expression.
pub fn evaluate(expr: &Expr, env: &mut dyn Environment) -> Result<Value, Error> {
    match expr {
        Expr::Literal(literal) => evaluate_literal(literal, env),
        Expr::NameRef(name) => {
            let name = name.name();
            env.lookup(name.text())
                .ok_or_else(|| Error::failed(format!("unknown name `{}`", name.text())))
        }
        Expr::Parenthesized(inner) => evaluate(&inner.expr(), env),
        Expr::If(choice) => {
            let (condition, then, otherwise) = choice.exprs();
            match boolean(&condition, env)? {
                true => evaluate(&then, env),
                false => evaluate(&otherwise, env),
            }
        }
        Expr::LogicalNot(not) => Ok(Value::Boolean(!boolean(&not.operand(), env)?)),
        Expr::Negation(negation) => match evaluate(&negation.operand(), env)? {
            Value::Int(number) => Ok(Value::Int(-number)),
            Value::Float(number) => Ok(Value::Float(-number)),
            value => Err(Error::failed(format!("cannot negate a {}", value.kind()))),
        },
        Expr::LogicalOr(or) => {
            let (left, right) = or.operands();
            Ok(Value::Boolean(
                boolean(&left, env)? || boolean(&right, env)?,
            ))
        }
        Expr::LogicalAnd(and) => {
            let (left, right) = and.operands();
            Ok(Value::Boolean(
                boolean(&left, env)? && boolean(&right, env)?,
            ))
        }
        Expr::Equality(equality) => compare(equality.operands(), Comparison::Equal, env),
        Expr::Inequality(inequality) => compare(inequality.operands(), Comparison::Unequal, env),
        Expr::Less(less) => compare(less.operands(), Comparison::Less, env),
        Expr::LessEqual(less) => compare(less.operands(), Comparison::LessEqual, env),
        Expr::Greater(greater) => compare(greater.operands(), Comparison::Greater, env),
        Expr::GreaterEqual(greater) => compare(greater.operands(), Comparison::GreaterEqual, env),
        Expr::Addition(sum) => arithmetic(sum.operands(), '+', env),
        Expr::Subtraction(difference) => arithmetic(difference.operands(), '-', env),
        Expr::Multiplication(product) => arithmetic(product.operands(), '*', env),
        Expr::Division(quotient) => arithmetic(quotient.operands(), '/', env),
        Expr::Modulo(remainder) => arithmetic(remainder.operands(), '%', env),
        Expr::Exponentiation(power) => arithmetic(power.operands(), '^', env),
        Expr::Call(call) => {
            let arguments = (call.arguments().map(|argument| evaluate(&argument, env)))
                .collect::<Result<Vec<_>, _>>()?;
            library::call(call.target().text(), arguments, env)
        }
        Expr::Index(index) => {
            let (target, position) = index.operands();
            let target = evaluate(&target, env)?;
            let position = evaluate(&position, env)?;
            element(target, position)
        }
        Expr::Access(access) => {
            let (target, name) = access.operands();
            member(evaluate(&target, env)?, name.text())
        }
    }
}

/// The value of a Boolean expression.
fn boolean(expr: &Expr, env: &mut dyn Environment) -> Result<bool, Error> {
    match evaluate(expr, env)? {
        Value::Boolean(value) => Ok(value),
        value => Err(Error::failed(format!(
            "a {} is not a Boolean",
            value.kind()
        ))),
    }
}

fn evaluate_literal(literal: &LiteralExpr, env: &mut dyn Environment) -> Result<Value, Error> {
    let range = || Error::failed("a number literal is out of range");
    Ok(match literal {
        LiteralExpr::Boolean(value) => Value::Boolean(value.value()),
        LiteralExpr::Integer(value) => Value::Int(value.value().ok_or_else(range)?),
        LiteralExpr::Float(value) => Value::Float(value.value().ok_or_else(range)?),
        LiteralExpr::String(text) => Value::String(string(text, env)?),
        LiteralExpr::Array(array) => Value::Array(
            (array.elements().map(|item| evaluate(&item, env))).collect::<Result<_, _>>()?,
        ),
        LiteralExpr::Pair(pair) => {
            let (left, right) = pair.exprs();
            Value::Pair(
                Box::new(evaluate(&left, env)?),
                Box::new(evaluate(&right, env)?),
            )
        }
        LiteralExpr::Map(map) => {
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for item in map.items() {
                let (key, value) = item.key_value();
                let key = evaluate(&key, env)?;
                let value = evaluate(&value, env)?;
                match entries.iter_mut().find(|(known, _)| equal(known, &key)) {
                    Some(entry) => entry.1 = value,
                    None => entries.push((key, value)),
                }
            }
            Value::Map(entries)
        }
        LiteralExpr::Object(object) => {
            let mut fields = IndexMap::new();
            for item in object.items() {
                let (name, value) = item.name_value();
                fields.insert(name.text().to_owned(), evaluate(&value, env)?);
            }
            Value::Object(fields)
        }
        LiteralExpr::Struct(record) => {
            let mut fields = IndexMap::new();
            for item in record.items() {
                let (name, value) = item.name_value();
                fields.insert(name.text().to_owned(), evaluate(&value, env)?);
            }
            Value::Object(fields)
        }
        LiteralExpr::None(_) => Value::None,
        LiteralExpr::Hints(_) | LiteralExpr::Input(_) | LiteralExpr::Output(_) => {
            return Err(Error::Unsupported(
                "hints, input and output literals are not supported".into(),
            ));
        }
    })
}

/// The text of a string literal, with its placeholders filled in.
pub fn string(literal: &LiteralString, env: &mut dyn Environment) -> Result<String, Error> {
    let mut text = String::new();
    match literal.strip_whitespace() {
        Some(parts) => {
            for part in parts {
                match part {
                    StrippedStringPart::Text(part) => text.push_str(&part),
                    StrippedStringPart::Placeholder(part) => placeholder(&part, env, &mut text)?,
                }
            }
        }
        None => {
            for part in literal.parts() {
                match part {
                    StringPart::Text(part) => part.unescape_to(&mut text),
                    StringPart::Placeholder(part) => placeholder(&part, env, &mut text)?,
                }
            }
        }
    }
    Ok(text)
}

/// The text of a command section, with the common indentation removed as WDL removes it.
pub fn command(section: &CommandSection, env: &mut dyn Environment) -> Result<String, Error> {
    let mut text = String::new();
    match section.strip_whitespace() {
        Some(parts) => {
            for part in parts {
                match part {
                    StrippedCommandPart::Text(part) => text.push_str(&part),
                    StrippedCommandPart::Placeholder(part) => placeholder(&part, env, &mut text)?,
                }
            }
        }
        None => {
            let heredoc = section.is_heredoc();
            for part in section.parts() {
                match part {
                    CommandPart::Text(part) => part.unescape_to(heredoc, &mut text),
                    CommandPart::Placeholder(part) => placeholder(&part, env, &mut text)?,
                }
            }
        }
    }
    Ok(text)
}

/// Writes the value of a placeholder: `None` writes nothing, or the `default` text.
fn placeholder(
    placeholder: &Placeholder,
    env: &mut dyn Environment,
    text: &mut String,
) -> Result<(), Error> {
    let value = evaluate(&placeholder.expr(), env)?;
    let option = placeholder.option();
    match (value, option) {
        (Value::None, Some(PlaceholderOption::Default(default))) => {
            text.push_str(&string(&default.value(), env)?);
        }
        (Value::None, _) => {}
        (Value::Boolean(value), Some(PlaceholderOption::TrueFalse(choice))) => {
            let (yes, no) = choice.values();
            text.push_str(&string(if value { &yes } else { &no }, env)?);
        }
        (Value::Array(items), Some(PlaceholderOption::Sep(separator))) => {
            let separator = string(&separator.separator(), env)?;
            let texts = (items.iter().map(|item| item.text()))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Error::failed("`sep` needs an array of primitive values"))?;
            text.push_str(&texts.join(&separator));
        }
        (value, _) => match value.text() {
            Some(part) => text.push_str(&part),
            None => {
                let kind = value.kind();
                return Err(Error::failed(format!(
                    "a {kind} cannot be put into a string"
                )));
            }
        },
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Equal,
    Unequal,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

fn compare(
    (left, right): (Expr, Expr),
    comparison: Comparison,
    env: &mut dyn Environment,
) -> Result<Value, Error> {
    let left = evaluate(&left, env)?;
    let right = evaluate(&right, env)?;
    let order = match comparison {
        Comparison::Equal => return Ok(Value::Boolean(equal(&left, &right))),
        Comparison::Unequal => return Ok(Value::Boolean(!equal(&left, &right))),
        _ => order(&left, &right).ok_or_else(|| {
            Error::failed(format!(
                "cannot compare a {} with a {}",
                left.kind(),
                right.kind()
            ))
        })?,
    };
    Ok(Value::Boolean(match comparison {
        Comparison::Less => order.is_lt(),
        Comparison::LessEqual => order.is_le(),
        Comparison::Greater => order.is_gt(),
        _ => order.is_ge(),
    }))
}

/// Whether two values are equal: numbers by value, files and strings by text.
pub fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| equal(a, b))
        }
        (Value::Pair(a, b), Value::Pair(c, d)) => equal(a, c) && equal(b, d),
        (Value::Map(left), Value::Map(right)) => {
            left.len() == right.len()
                && (left.iter().zip(right)).all(|((a, b), (c, d))| equal(a, c) && equal(b, d))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && (left.iter()).all(|(name, a)| right.get(name).is_some_and(|b| equal(a, b)))
        }
        _ => match order(left, right) {
            Some(order) => order.is_eq(),
            None => left == right,
        },
    }
}

fn order(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            number(left)?.partial_cmp(&number(right)?)
        }
        (Value::Boolean(a), Value::Boolean(b)) => Some(a.cmp(b)),
        (
            Value::String(a) | Value::File(a) | Value::Directory(a),
            Value::String(b) | Value::File(b) | Value::Directory(b),
        ) => Some(a.cmp(b)),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        _ => None,
    }
}

fn arithmetic(
    (left, right): (Expr, Expr),
    operator: char,
    env: &mut dyn Environment,
) -> Result<Value, Error> {
    let left = evaluate(&left, env)?;
    let right = evaluate(&right, env)?;
    let fail = || {
        let (left, right) = (left.kind(), right.kind());
        Error::failed(format!(
            "cannot apply `{operator}` to a {left} and a {right}"
        ))
    };
    match (&left, &right) {
        (Value::Int(a), Value::Int(b)) => {
            let (a, b) = (*a, *b);
            let result = match operator {
                '+' => a.checked_add(b),
                '-' => a.checked_sub(b),
                '*' => a.checked_mul(b),
                '/' if b == 0 => return Err(Error::failed("division by zero")),
                '/' => a.checked_div(b),
                '%' if b == 0 => return Err(Error::failed("division by zero")),
                '%' => a.checked_rem(b),
                _ => u32::try_from(b).ok().and_then(|b| a.checked_pow(b)),
            };
            result
                .map(Value::Int)
                .ok_or_else(|| Error::failed("an integer overflows"))
        }
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (a, b) = (
                number(&left).unwrap_or_default(),
                number(&right).unwrap_or_default(),
            );
            Ok(Value::Float(match operator {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' => a / b,
                '%' => a % b,
                _ => a.powf(b),
            }))
        }
        (Value::String(_) | Value::File(_), _) | (_, Value::String(_)) if operator == '+' => {
            let joined = format!(
                "{}{}",
                left.text().ok_or_else(fail)?,
                right.text().ok_or_else(fail)?
            );
            Ok(match left {
                Value::File(_) => Value::File(joined),
                _ => Value::String(joined),
            })
        }
        _ => Err(fail()),
    }
}

/// The element of an array at a position, or the value of a map for a key.
fn element(target: Value, position: Value) -> Result<Value, Error> {
    match (target, position) {
        (Value::Array(items), Value::Int(index)) => {
            let count = items.len();
            (usize::try_from(index)
                .ok()
                .and_then(|index| items.into_iter().nth(index)))
            .ok_or_else(|| Error::failed(format!("index {index} is out of bounds for {count}")))
        }
        (Value::Map(entries), key) => (entries.into_iter())
            .find(|(known, _)| equal(known, &key))
            .map(|(_, value)| value)
            .ok_or_else(|| Error::failed(format!("the map has no key {}", key.to_json()))),
        (target, _) => Err(Error::failed(format!(
            "a {} cannot be indexed",
            target.kind()
        ))),
    }
}

/// A member of an object, a struct or the outputs of a call, or a side of a pair.
fn member(target: Value, name: &str) -> Result<Value, Error> {
    match (target, name) {
        (Value::Pair(left, _), "left") => Ok(*left),
        (Value::Pair(_, right), "right") => Ok(*right),
        (Value::Object(mut fields), _) => {
            (fields.shift_remove(name)).ok_or_else(|| Error::failed(format!("no member `{name}`")))
        }
        (target, _) => Err(Error::failed(format!(
            "a {} has no member `{name}`",
            target.kind()
        ))),
    }
}
