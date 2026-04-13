//! An evaluator for expr-lang (github.com/expr-lang/expr v1.17), the language of Argo's
//! expression tags `{{=...}}`, of `depends` and of the other expression fields. Argo adds
//! `asInt`, `asFloat`, `toJson`, `jsonpath` and the Sprig functions as `sprig.<name>(...)`.
//!
//! The evaluator follows expr's runtime rules: `/` always gives a float, `&&` and `||` take
//! only booleans, a missing map key reads as nil, and an unknown top-level name is an error,
//! which makes Argo leave the tag for a later substitution phase.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;

use crate::jsonpath;
use crate::sprig;
use crate::value::Value;

/// Why an expression cannot be parsed or run.
#[derive(Clone, Debug, PartialEq)]
pub struct ExprError {
    pub message: String,
}

impl ExprError {
    pub fn new(message: impl Into<String>) -> Self {
        ExprError {
            message: message.into(),
        }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExprError {}

type Result<T> = std::result::Result<T, ExprError>;

/// Parses and runs `source` with the variables of `env`.
pub fn evaluate(source: &str, env: &BTreeMap<String, Value>) -> Result<Value> {
    let expression = parse(source)?;
    Evaluator {
        env,
        locals: Vec::new(),
        pointers: Vec::new(),
    }
    .eval(&expression)
}

/// Runs `source` and requires a boolean, as Argo's `EvalBool` does.
pub fn evaluate_bool(source: &str, env: &BTreeMap<String, Value>) -> Result<bool> {
    match evaluate(source, env)? {
        Value::Bool(flag) => Ok(flag),
        other => Err(ExprError::new(format!(
            "expected a boolean from `{source}`, got {}",
            other.to_go_string()
        ))),
    }
}

/// The names an expression reads at its top level, like `inputs` and `retries`.
pub fn identifiers(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(expression) = parse(source) {
        collect_identifiers(&expression, &mut names);
    }
    names
}

fn collect_identifiers(expression: &Expr, names: &mut Vec<String>) {
    match expression {
        Expr::Ident(name) => names.push(name.clone()),
        Expr::Array(items) => {
            for item in items {
                collect_identifiers(item, names);
            }
        }
        Expr::Map(entries) => {
            for (key, value) in entries {
                collect_identifiers(key, names);
                collect_identifiers(value, names);
            }
        }
        Expr::Member {
            object, property, ..
        } => {
            collect_identifiers(object, names);
            collect_identifiers(property, names);
        }
        Expr::Slice { object, from, to } => {
            collect_identifiers(object, names);
            for bound in [from, to].into_iter().flatten() {
                collect_identifiers(bound, names);
            }
        }
        Expr::Call { callee, arguments } => {
            if let Callee::Method(object, _) = callee {
                collect_identifiers(object, names);
            }
            for argument in arguments {
                collect_identifiers(argument, names);
            }
        }
        Expr::Closure(body) | Expr::Unary(_, body) => collect_identifiers(body, names),
        Expr::Binary(_, left, right) => {
            collect_identifiers(left, names);
            collect_identifiers(right, names);
        }
        Expr::Conditional(condition, then, otherwise) => {
            collect_identifiers(condition, names);
            collect_identifiers(then, names);
            collect_identifiers(otherwise, names);
        }
        Expr::Let(_, value, body) => {
            collect_identifiers(value, names);
            collect_identifiers(body, names);
        }
        Expr::Literal(_) | Expr::Pointer(_) => {}
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    End,
}

const OPERATORS: [&str; 34] = [
    "?.", "??", "..", "**", "==", "!=", "<=", ">=", "&&", "||", "::", "!", "<", ">", "+", "-", "*",
    "/", "%", "^", "|", "?", ":", ".", ",", "(", ")", "[", "]", "{", "}", "#", "=", ";",
];

fn lex(source: &str) -> Result<Vec<Token>> {
    let characters: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
        } else if character.is_ascii_digit() {
            let (token, next) = lex_number(&characters, index)?;
            tokens.push(token);
            index = next;
        } else if character == '"' || character == '\'' {
            let (text, next) = lex_string(&characters, index)?;
            tokens.push(Token::Str(text));
            index = next;
        } else if character == '`' {
            let end = characters[index + 1..]
                .iter()
                .position(|c| *c == '`')
                .ok_or_else(|| ExprError::new("literal not terminated"))?;
            tokens.push(Token::Str(
                characters[index + 1..index + 1 + end].iter().collect(),
            ));
            index += end + 2;
        } else if character.is_alphabetic() || character == '_' || character == '$' {
            let start = index;
            while index < characters.len()
                && (characters[index].is_alphanumeric()
                    || characters[index] == '_'
                    || characters[index] == '$')
            {
                index += 1;
            }
            tokens.push(Token::Ident(characters[start..index].iter().collect()));
        } else {
            let rest: String = characters[index..characters.len().min(index + 2)]
                .iter()
                .collect();
            let operator = OPERATORS
                .iter()
                .find(|operator| rest.starts_with(**operator))
                .ok_or_else(|| ExprError::new(format!("unexpected character `{character}`")))?;
            // `?.` before a digit is a conditional followed by a number, like `a?.5:1`.
            if *operator == "?."
                && characters
                    .get(index + 2)
                    .is_some_and(|next| next.is_ascii_digit())
            {
                tokens.push(Token::Op("?"));
                index += 1;
                continue;
            }
            tokens.push(Token::Op(operator));
            index += operator.len();
        }
    }
    tokens.push(Token::End);
    Ok(tokens)
}

fn lex_number(characters: &[char], start: usize) -> Result<(Token, usize)> {
    let mut index = start;
    let radix = match (characters[index], characters.get(index + 1)) {
        ('0', Some('x' | 'X')) => 16,
        ('0', Some('o' | 'O')) => 8,
        ('0', Some('b' | 'B')) => 2,
        _ => 10,
    };
    if radix != 10 {
        index += 2;
        let digits_start = index;
        while index < characters.len()
            && (characters[index].is_digit(radix) || characters[index] == '_')
        {
            index += 1;
        }
        let digits: String = characters[digits_start..index]
            .iter()
            .filter(|c| **c != '_')
            .collect();
        let number = i64::from_str_radix(&digits, radix)
            .map_err(|_| ExprError::new(format!("invalid number `{digits}`")))?;
        return Ok((Token::Int(number), index));
    }
    let mut float = false;
    while index < characters.len()
        && (characters[index].is_ascii_digit() || characters[index] == '_')
    {
        index += 1;
    }
    // A dot starts a fraction only before a digit, so `1..3` stays a range.
    if characters.get(index) == Some(&'.')
        && characters
            .get(index + 1)
            .is_some_and(|next| next.is_ascii_digit())
    {
        float = true;
        index += 1;
        while index < characters.len() && characters[index].is_ascii_digit() {
            index += 1;
        }
    }
    if matches!(characters.get(index), Some('e' | 'E')) {
        let mut next = index + 1;
        if matches!(characters.get(next), Some('+' | '-')) {
            next += 1;
        }
        if characters.get(next).is_some_and(|c| c.is_ascii_digit()) {
            float = true;
            index = next;
            while index < characters.len() && characters[index].is_ascii_digit() {
                index += 1;
            }
        }
    }
    let text: String = characters[start..index]
        .iter()
        .filter(|c| **c != '_')
        .collect();
    let token = if float {
        Token::Float(
            text.parse()
                .map_err(|_| ExprError::new(format!("invalid number `{text}`")))?,
        )
    } else {
        match text.parse::<i64>() {
            Ok(number) => Token::Int(number),
            Err(_) => Token::Float(
                text.parse()
                    .map_err(|_| ExprError::new(format!("invalid number `{text}`")))?,
            ),
        }
    };
    Ok((token, index))
}

fn lex_string(characters: &[char], start: usize) -> Result<(String, usize)> {
    let quote = characters[start];
    let mut text = String::new();
    let mut index = start + 1;
    loop {
        let Some(&character) = characters.get(index) else {
            return Err(ExprError::new("literal not terminated"));
        };
        index += 1;
        if character == quote {
            return Ok((text, index));
        }
        if character != '\\' {
            text.push(character);
            continue;
        }
        let Some(&escape) = characters.get(index) else {
            return Err(ExprError::new("literal not terminated"));
        };
        index += 1;
        let hex = |count: usize, index: &mut usize| -> Result<char> {
            let digits: String = characters
                .get(*index..*index + count)
                .ok_or_else(|| ExprError::new("invalid escape"))?
                .iter()
                .collect();
            *index += count;
            u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| ExprError::new(format!("invalid escape `{digits}`")))
        };
        match escape {
            'n' => text.push('\n'),
            't' => text.push('\t'),
            'r' => text.push('\r'),
            'a' => text.push('\u{7}'),
            'b' => text.push('\u{8}'),
            'f' => text.push('\u{c}'),
            'v' => text.push('\u{b}'),
            'x' => text.push(hex(2, &mut index)?),
            'u' => text.push(hex(4, &mut index)?),
            'U' => text.push(hex(8, &mut index)?),
            '0'..='7' => {
                let digits: String = characters[index - 1..(index + 2).min(characters.len())]
                    .iter()
                    .collect();
                index += 2;
                let code = u32::from_str_radix(&digits, 8)
                    .map_err(|_| ExprError::new("invalid escape"))?;
                text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            other => text.push(other),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Literal(Value),
    Array(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Ident(String),
    /// `#` or `#name` in a closure: the element, or a named variable of the closure.
    Pointer(Option<String>),
    Member {
        object: Box<Expr>,
        property: Box<Expr>,
        optional: bool,
    },
    Slice {
        object: Box<Expr>,
        from: Option<Box<Expr>>,
        to: Option<Box<Expr>>,
    },
    Call {
        callee: Callee,
        arguments: Vec<Expr>,
    },
    Closure(Box<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Let(String, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum Callee {
    Function(String),
    Method(Box<Expr>, String),
}

/// Builtins whose second argument is a closure over the elements of the first.
const PREDICATES: [&str; 15] = [
    "all",
    "any",
    "none",
    "one",
    "filter",
    "map",
    "count",
    "sum",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "groupBy",
    "sortBy",
    "reduce",
];

const WORD_OPERATORS: [&str; 7] = [
    "and",
    "or",
    "in",
    "matches",
    "contains",
    "startsWith",
    "endsWith",
];

fn binary_precedence(operator: &str) -> Option<(u16, bool)> {
    Some(match operator {
        "|" => (0, false),
        "or" | "||" => (10, false),
        "and" | "&&" => (15, false),
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "in" | "matches" | "contains" | "startsWith"
        | "endsWith" => (20, false),
        ".." => (25, false),
        "+" | "-" => (30, false),
        "*" | "/" | "%" => (60, false),
        "**" | "^" => (100, true),
        "??" => (500, false),
        _ => return None,
    })
}

fn parse(source: &str) -> Result<Expr> {
    let mut parser = Parser {
        tokens: lex(source)?,
        position: 0,
        depth: 0,
    };
    let expression = parser.expression(0)?;
    if parser.peek() != &Token::End {
        return Err(ExprError::new(format!(
            "unexpected token {:?} in `{source}`",
            parser.peek()
        )));
    }
    Ok(expression)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    /// How many closures enclose the current position, where `#` and `.name` are allowed.
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.position.min(self.tokens.len() - 1)]
    }

    fn peek_at(&self, offset: usize) -> &Token {
        &self.tokens[(self.position + offset).min(self.tokens.len() - 1)]
    }

    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        if self.position < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn is_op(&self, operator: &str) -> bool {
        matches!(self.peek(), Token::Op(op) if *op == operator)
    }

    fn eat_op(&mut self, operator: &str) -> bool {
        let found = self.is_op(operator);
        if found {
            self.position += 1;
        }
        found
    }

    fn expect_op(&mut self, operator: &str) -> Result<()> {
        if self.eat_op(operator) {
            Ok(())
        } else {
            Err(ExprError::new(format!(
                "expected `{operator}`, found {:?}",
                self.peek()
            )))
        }
    }

    fn expression(&mut self, precedence: u16) -> Result<Expr> {
        if precedence == 0 && matches!(self.peek(), Token::Ident(word) if word == "let") {
            return self.let_binding();
        }
        let mut left = self.primary()?;
        loop {
            let (operator, negate) = match self.peek().clone() {
                Token::Op(op) if binary_precedence(op).is_some() => (op.to_owned(), false),
                Token::Ident(word) if WORD_OPERATORS.contains(&word.as_str()) => (word, false),
                Token::Ident(word) if word == "not" => match self.peek_at(1) {
                    Token::Ident(next)
                        if ["in", "matches", "contains", "startsWith", "endsWith"]
                            .contains(&next.as_str()) =>
                    {
                        (next.clone(), true)
                    }
                    _ => break,
                },
                _ => break,
            };
            let Some((operator_precedence, right)) = binary_precedence(&operator) else {
                break;
            };
            if operator_precedence < precedence {
                break;
            }
            self.position += if negate { 2 } else { 1 };
            if operator == "|" {
                let Token::Ident(name) = self.next() else {
                    return Err(ExprError::new("expected a function name after `|`"));
                };
                let mut arguments = vec![left];
                if self.is_op("(") {
                    arguments.extend(self.arguments(&name, 1)?);
                }
                left = Expr::Call {
                    callee: Callee::Function(name),
                    arguments,
                };
                continue;
            }
            let right = if right {
                self.expression(operator_precedence)?
            } else {
                self.expression(operator_precedence + 1)?
            };
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
            if negate {
                left = Expr::Unary("!", Box::new(left));
            }
        }
        if precedence == 0 {
            while self.eat_op("?") {
                let then = self.expression(0)?;
                self.expect_op(":")?;
                let otherwise = self.expression(0)?;
                left = Expr::Conditional(Box::new(left), Box::new(then), Box::new(otherwise));
            }
        }
        Ok(left)
    }

    fn let_binding(&mut self) -> Result<Expr> {
        self.next();
        let Token::Ident(name) = self.next() else {
            return Err(ExprError::new("expected a name after `let`"));
        };
        self.expect_op("=")?;
        let value = self.expression(0)?;
        self.expect_op(";")?;
        let body = self.expression(0)?;
        Ok(Expr::Let(name, Box::new(value), Box::new(body)))
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.peek().clone() {
            Token::Op("!") => {
                self.next();
                let operand = self.expression(50)?;
                self.postfix(Expr::Unary("!", Box::new(operand)))
            }
            Token::Ident(word) if word == "not" => {
                self.next();
                let operand = self.expression(50)?;
                self.postfix(Expr::Unary("!", Box::new(operand)))
            }
            Token::Op(op @ ("-" | "+")) => {
                self.next();
                let operand = self.expression(90)?;
                self.postfix(Expr::Unary(op, Box::new(operand)))
            }
            Token::Op("(") => {
                self.next();
                let inner = self.expression(0)?;
                self.expect_op(")")?;
                self.postfix(inner)
            }
            Token::Op("#") if self.depth > 0 => {
                self.next();
                let name = match self.peek().clone() {
                    Token::Ident(name) => {
                        self.next();
                        Some(name)
                    }
                    _ => None,
                };
                self.postfix(Expr::Pointer(name))
            }
            Token::Op(".") if self.depth > 0 => self.postfix(Expr::Pointer(None)),
            _ => self.secondary(),
        }
    }

    fn secondary(&mut self) -> Result<Expr> {
        let node = match self.next() {
            Token::Ident(name) => match name.as_str() {
                "true" => Expr::Literal(Value::Bool(true)),
                "false" => Expr::Literal(Value::Bool(false)),
                "nil" => Expr::Literal(Value::Nil),
                _ if self.is_op("(") => {
                    let arguments = self.arguments(&name, 0)?;
                    Expr::Call {
                        callee: Callee::Function(name),
                        arguments,
                    }
                }
                _ => Expr::Ident(name),
            },
            Token::Int(number) => Expr::Literal(Value::Int(number)),
            Token::Float(number) => Expr::Literal(Value::Float(number)),
            Token::Str(text) => Expr::Literal(Value::String(text)),
            Token::Op("[") => {
                let mut items = Vec::new();
                while !self.is_op("]") {
                    items.push(self.expression(0)?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("]")?;
                Expr::Array(items)
            }
            Token::Op("{") => {
                let mut entries = Vec::new();
                while !self.is_op("}") {
                    let key = match self.next() {
                        Token::Ident(name) => Expr::Literal(Value::String(name)),
                        Token::Str(text) => Expr::Literal(Value::String(text)),
                        Token::Int(number) => Expr::Literal(Value::String(number.to_string())),
                        Token::Op("(") => {
                            let key = self.expression(0)?;
                            self.expect_op(")")?;
                            key
                        }
                        other => {
                            return Err(ExprError::new(format!("unexpected map key {other:?}")));
                        }
                    };
                    self.expect_op(":")?;
                    entries.push((key, self.expression(0)?));
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("}")?;
                Expr::Map(entries)
            }
            other => return Err(ExprError::new(format!("unexpected token {other:?}"))),
        };
        self.postfix(node)
    }

    /// The arguments of a call, from `(` to `)`. `skipped` counts arguments already given,
    /// like the left side of a pipe.
    fn arguments(&mut self, name: &str, skipped: usize) -> Result<Vec<Expr>> {
        self.expect_op("(")?;
        let mut arguments = Vec::new();
        while !self.is_op(")") {
            let index = arguments.len() + skipped;
            if PREDICATES.contains(&name) && index == 1 {
                self.depth += 1;
                let body = if self.eat_op("{") {
                    let body = self.expression(0)?;
                    self.expect_op("}")?;
                    body
                } else {
                    self.expression(0)?
                };
                self.depth -= 1;
                arguments.push(Expr::Closure(Box::new(body)));
            } else {
                arguments.push(self.expression(0)?);
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(arguments)
    }

    fn postfix(&mut self, mut node: Expr) -> Result<Expr> {
        loop {
            if self.is_op(".") || self.is_op("?.") {
                let optional = self.is_op("?.");
                self.next();
                if optional && self.is_op("[") {
                    node = self.index(node, true)?;
                    continue;
                }
                let name = match self.next() {
                    Token::Ident(name) => name,
                    other => {
                        return Err(ExprError::new(format!(
                            "expected a name after `.`, found {other:?}"
                        )));
                    }
                };
                if self.is_op("(") {
                    let arguments = self.arguments(&name, 0)?;
                    node = Expr::Call {
                        callee: Callee::Method(Box::new(node), name),
                        arguments,
                    };
                } else {
                    node = Expr::Member {
                        object: Box::new(node),
                        property: Box::new(Expr::Literal(Value::String(name))),
                        optional,
                    };
                }
            } else if self.is_op("[") {
                node = self.index(node, false)?;
            } else {
                return Ok(node);
            }
        }
    }

    fn index(&mut self, object: Expr, optional: bool) -> Result<Expr> {
        self.expect_op("[")?;
        if self.eat_op(":") {
            let to = if self.is_op("]") {
                None
            } else {
                Some(Box::new(self.expression(0)?))
            };
            self.expect_op("]")?;
            return Ok(Expr::Slice {
                object: Box::new(object),
                from: None,
                to,
            });
        }
        let index = self.expression(0)?;
        if self.eat_op(":") {
            let to = if self.is_op("]") {
                None
            } else {
                Some(Box::new(self.expression(0)?))
            };
            self.expect_op("]")?;
            return Ok(Expr::Slice {
                object: Box::new(object),
                from: Some(Box::new(index)),
                to,
            });
        }
        self.expect_op("]")?;
        Ok(Expr::Member {
            object: Box::new(object),
            property: Box::new(index),
            optional,
        })
    }
}

struct Evaluator<'a> {
    env: &'a BTreeMap<String, Value>,
    locals: Vec<(String, Value)>,
    /// The element and index of each enclosing closure.
    pointers: Vec<(Value, usize)>,
}

impl Evaluator<'_> {
    fn eval(&mut self, expression: &Expr) -> Result<Value> {
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Array(items) => Ok(Value::Array(
                items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<_>>()?,
            )),
            Expr::Map(entries) => {
                let mut map = BTreeMap::new();
                for (key, value) in entries {
                    let key = match self.eval(key)? {
                        Value::String(text) => text,
                        other => other.to_go_string(),
                    };
                    map.insert(key, self.eval(value)?);
                }
                Ok(Value::Map(map))
            }
            Expr::Ident(name) => self.lookup(name),
            Expr::Pointer(name) => {
                let Some((element, index)) = self.pointers.last() else {
                    return Err(ExprError::new("`#` outside of a closure"));
                };
                match name.as_deref() {
                    None => Ok(element.clone()),
                    Some("index") => Ok(Value::Int(*index as i64)),
                    Some(other) => Err(ExprError::new(format!("unknown pointer #{other}"))),
                }
            }
            Expr::Member {
                object,
                property,
                optional,
            } => {
                let object = self.eval(object)?;
                let property = self.eval(property)?;
                member(&object, &property, *optional)
            }
            Expr::Slice { object, from, to } => {
                let object = self.eval(object)?;
                let from = match from {
                    Some(from) => Some(self.eval(from)?),
                    None => None,
                };
                let to = match to {
                    Some(to) => Some(self.eval(to)?),
                    None => None,
                };
                slice(&object, from.as_ref(), to.as_ref())
            }
            Expr::Call { callee, arguments } => self.call(callee, arguments),
            Expr::Closure(_) => Err(ExprError::new("a closure can only be a predicate")),
            Expr::Unary(operator, operand) => {
                let value = self.eval(operand)?;
                match (*operator, value) {
                    ("!", Value::Bool(flag)) => Ok(Value::Bool(!flag)),
                    ("-", Value::Int(number)) => Ok(Value::Int(number.wrapping_neg())),
                    ("-", Value::Float(number)) => Ok(Value::Float(-number)),
                    ("+", value @ (Value::Int(_) | Value::Float(_))) => Ok(value),
                    (operator, value) => Err(ExprError::new(format!(
                        "invalid operation: {operator}({})",
                        value.type_name()
                    ))),
                }
            }
            Expr::Binary(operator, left, right) => self.binary(operator, left, right),
            Expr::Conditional(condition, then, otherwise) => match self.eval(condition)? {
                Value::Bool(true) => self.eval(then),
                Value::Bool(false) => self.eval(otherwise),
                other => Err(ExprError::new(format!(
                    "non-bool expression (type {}) used as condition",
                    other.type_name()
                ))),
            },
            Expr::Let(name, value, body) => {
                let value = self.eval(value)?;
                self.locals.push((name.clone(), value));
                let result = self.eval(body);
                self.locals.pop();
                result
            }
        }
    }

    fn lookup(&self, name: &str) -> Result<Value> {
        if let Some((_, value)) = self.locals.iter().rev().find(|(local, _)| local == name) {
            return Ok(value.clone());
        }
        self.env
            .get(name)
            .cloned()
            .ok_or_else(|| ExprError::new(format!("unknown name {name}")))
    }

    fn binary(&mut self, operator: &str, left: &Expr, right: &Expr) -> Result<Value> {
        match operator {
            "or" | "||" | "and" | "&&" => {
                let or = matches!(operator, "or" | "||");
                let Value::Bool(first) = self.eval(left)? else {
                    return Err(ExprError::new(format!(
                        "invalid operation: {operator} needs booleans"
                    )));
                };
                if first == or {
                    return Ok(Value::Bool(first));
                }
                match self.eval(right)? {
                    Value::Bool(second) => Ok(Value::Bool(second)),
                    _ => Err(ExprError::new(format!(
                        "invalid operation: {operator} needs booleans"
                    ))),
                }
            }
            "??" => match self.eval(left) {
                Ok(Value::Nil) | Err(_) => self.eval(right),
                Ok(value) => Ok(value),
            },
            _ => {
                let left = self.eval(left)?;
                let right = self.eval(right)?;
                binary(operator, &left, &right)
            }
        }
    }

    fn call(&mut self, callee: &Callee, arguments: &[Expr]) -> Result<Value> {
        let name = match callee {
            Callee::Function(name) => name.as_str(),
            Callee::Method(object, name) => {
                if matches!(object.as_ref(), Expr::Ident(base) if base == "sprig")
                    && !self.env.contains_key("sprig")
                {
                    let values = arguments
                        .iter()
                        .map(|argument| self.eval(argument))
                        .collect::<Result<Vec<_>>>()?;
                    return sprig::call(name, values);
                }
                return Err(ExprError::new(format!("method {name} is not supported")));
            }
        };
        if PREDICATES.contains(&name) {
            return self.predicate(name, arguments);
        }
        let values = arguments
            .iter()
            .map(|argument| self.eval(argument))
            .collect::<Result<Vec<_>>>()?;
        builtin(name, values)
    }

    fn predicate(&mut self, name: &str, arguments: &[Expr]) -> Result<Value> {
        let (Some(first), Some(Expr::Closure(body))) = (arguments.first(), arguments.get(1)) else {
            return match name {
                "count" | "sum" if arguments.len() == 1 => {
                    let value = self.eval(&arguments[0])?;
                    let items = array(&value, name)?;
                    if name == "count" {
                        Ok(Value::Int(
                            items
                                .iter()
                                .filter(|item| **item == Value::Bool(true))
                                .count() as i64,
                        ))
                    } else {
                        items
                            .iter()
                            .try_fold(Value::Int(0), |total, item| binary("+", &total, item))
                    }
                }
                _ => Err(ExprError::new(format!(
                    "{name} needs an array and a closure"
                ))),
            };
        };
        let collection = self.eval(first)?;
        let items = array(&collection, name)?.to_vec();
        let mut results = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            self.pointers.push((item.clone(), index));
            let result = self.eval(body);
            self.pointers.pop();
            results.push(result?);
        }
        let flag = |value: &Value| match value {
            Value::Bool(flag) => Ok(*flag),
            other => Err(ExprError::new(format!(
                "{name}: the predicate gave {} instead of a boolean",
                other.type_name()
            ))),
        };
        let flags = || results.iter().map(flag).collect::<Result<Vec<bool>>>();
        Ok(match name {
            "all" => Value::Bool(flags()?.iter().all(|flag| *flag)),
            "any" => Value::Bool(flags()?.iter().any(|flag| *flag)),
            "none" => Value::Bool(!flags()?.iter().any(|flag| *flag)),
            "one" => Value::Bool(flags()?.iter().filter(|flag| **flag).count() == 1),
            "count" => Value::Int(flags()?.iter().filter(|flag| **flag).count() as i64),
            "filter" => Value::Array(
                items
                    .into_iter()
                    .zip(flags()?)
                    .filter(|(_, keep)| *keep)
                    .map(|(item, _)| item)
                    .collect(),
            ),
            "map" => Value::Array(results),
            "sum" => results
                .iter()
                .try_fold(Value::Int(0), |total, item| binary("+", &total, item))?,
            "find" | "findLast" | "findIndex" | "findLastIndex" => {
                let flags = flags()?;
                let position = if name.starts_with("findLast") {
                    flags.iter().rposition(|flag| *flag)
                } else {
                    flags.iter().position(|flag| *flag)
                };
                match (name.ends_with("Index"), position) {
                    (true, Some(index)) => Value::Int(index as i64),
                    (true, None) => Value::Int(-1),
                    (false, Some(index)) => items[index].clone(),
                    (false, None) => Value::Nil,
                }
            }
            "groupBy" => {
                let mut groups: BTreeMap<String, Value> = BTreeMap::new();
                for (item, key) in items.into_iter().zip(results) {
                    let key = match key {
                        Value::String(text) => text,
                        other => other.to_go_string(),
                    };
                    if let Value::Array(group) = groups
                        .entry(key)
                        .or_insert_with(|| Value::Array(Vec::new()))
                    {
                        group.push(item);
                    }
                }
                Value::Map(groups)
            }
            "sortBy" => {
                let mut pairs: Vec<(Value, Value)> = results.into_iter().zip(items).collect();
                let mut failure = None;
                pairs.sort_by(|(a, _), (b, _)| {
                    compare(a, b).unwrap_or_else(|error| {
                        failure = Some(error);
                        std::cmp::Ordering::Equal
                    })
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                Value::Array(pairs.into_iter().map(|(_, item)| item).collect())
            }
            _ => return Err(ExprError::new(format!("{name} is not supported"))),
        })
    }
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value]> {
    match value {
        Value::Array(items) => Ok(items),
        Value::Nil => Ok(&[]),
        other => Err(ExprError::new(format!(
            "{name}: expected an array, got {}",
            other.type_name()
        ))),
    }
}

fn member(object: &Value, property: &Value, optional: bool) -> Result<Value> {
    match (object, property) {
        (Value::Nil, _) if optional => Ok(Value::Nil),
        (Value::Map(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil)),
        (Value::Map(map), other) => Ok(map
            .get(&other.to_go_string())
            .cloned()
            .unwrap_or(Value::Nil)),
        (Value::Array(items), index) => {
            let index = integer(index)?;
            let position = if index < 0 {
                items.len() as i64 + index
            } else {
                index
            };
            items
                .get(usize::try_from(position).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| {
                    ExprError::new(format!(
                        "index out of range: {index} (array length is {})",
                        items.len()
                    ))
                })
        }
        (Value::String(text), index) => {
            let characters: Vec<char> = text.chars().collect();
            let index = integer(index)?;
            let position = if index < 0 {
                characters.len() as i64 + index
            } else {
                index
            };
            characters
                .get(usize::try_from(position).unwrap_or(usize::MAX))
                .map(|character| Value::String(character.to_string()))
                .ok_or_else(|| ExprError::new(format!("index out of range: {index}")))
        }
        (object, property) => Err(ExprError::new(format!(
            "cannot fetch {} from {}",
            property.to_go_string(),
            object.type_name()
        ))),
    }
}

fn integer(value: &Value) -> Result<i64> {
    match value {
        Value::Int(number) => Ok(*number),
        Value::Float(number) if number.fract() == 0.0 => Ok(*number as i64),
        other => Err(ExprError::new(format!(
            "cannot use {} as an index",
            other.type_name()
        ))),
    }
}

fn slice(object: &Value, from: Option<&Value>, to: Option<&Value>) -> Result<Value> {
    let length = match object {
        Value::Array(items) => items.len(),
        Value::String(text) => text.chars().count(),
        other => {
            return Err(ExprError::new(format!(
                "cannot slice {}",
                other.type_name()
            )));
        }
    } as i64;
    let bound = |value: Option<&Value>, default: i64| -> Result<usize> {
        let index = match value {
            Some(value) => integer(value)?,
            None => default,
        };
        let index = if index < 0 { length + index } else { index };
        Ok(index.clamp(0, length) as usize)
    };
    let start = bound(from, 0)?;
    let end = bound(to, length)?.max(start);
    Ok(match object {
        Value::Array(items) => Value::Array(items[start..end].to_vec()),
        Value::String(text) => Value::String(text.chars().skip(start).take(end - start).collect()),
        _ => Value::Nil,
    })
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        _ => None,
    }
}

/// Equality as expr's `runtime.Equal`: numbers compare by value across int and float.
pub fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            number(left) == number(right)
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((ka, va), (kb, vb))| ka == kb && equal(va, vb))
        }
        _ => left == right,
    }
}

fn compare(left: &Value, right: &Value) -> Result<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
        _ => match (number(left), number(right)) {
            (Some(a), Some(b)) => a
                .partial_cmp(&b)
                .ok_or_else(|| ExprError::new("cannot compare NaN")),
            _ => Err(ExprError::new(format!(
                "invalid operation: cannot compare {} and {}",
                left.type_name(),
                right.type_name()
            ))),
        },
    }
}

fn binary(operator: &str, left: &Value, right: &Value) -> Result<Value> {
    use std::cmp::Ordering;
    let mismatch = || {
        ExprError::new(format!(
            "invalid operation: {} {operator} {} (mismatched types {} and {})",
            left.to_go_string(),
            right.to_go_string(),
            left.type_name(),
            right.type_name()
        ))
    };
    Ok(match operator {
        "==" => Value::Bool(equal(left, right)),
        "!=" => Value::Bool(!equal(left, right)),
        "<" => Value::Bool(compare(left, right)? == Ordering::Less),
        ">" => Value::Bool(compare(left, right)? == Ordering::Greater),
        "<=" => Value::Bool(compare(left, right)? != Ordering::Greater),
        ">=" => Value::Bool(compare(left, right)? != Ordering::Less),
        "+" => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_add(*b)),
            (Value::String(a), Value::String(b)) => Value::String(format!("{a}{b}")),
            (Value::Array(a), Value::Array(b)) => {
                Value::Array(a.iter().chain(b).cloned().collect())
            }
            _ => Value::Float(
                number(left).ok_or_else(mismatch)? + number(right).ok_or_else(mismatch)?,
            ),
        },
        "-" => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_sub(*b)),
            _ => Value::Float(
                number(left).ok_or_else(mismatch)? - number(right).ok_or_else(mismatch)?,
            ),
        },
        "*" => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_mul(*b)),
            _ => Value::Float(
                number(left).ok_or_else(mismatch)? * number(right).ok_or_else(mismatch)?,
            ),
        },
        "/" => {
            Value::Float(number(left).ok_or_else(mismatch)? / number(right).ok_or_else(mismatch)?)
        }
        "%" => match (left, right) {
            (Value::Int(_), Value::Int(0)) => {
                return Err(ExprError::new("integer divide by zero"));
            }
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_rem(*b)),
            _ => return Err(mismatch()),
        },
        "**" | "^" => Value::Float(
            number(left)
                .ok_or_else(mismatch)?
                .powf(number(right).ok_or_else(mismatch)?),
        ),
        ".." => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Array((*a..=*b).map(Value::Int).collect()),
            _ => return Err(mismatch()),
        },
        "in" => match right {
            Value::Array(items) => Value::Bool(items.iter().any(|item| equal(item, left))),
            Value::Map(map) => match left {
                Value::String(key) => Value::Bool(map.contains_key(key)),
                _ => return Err(mismatch()),
            },
            Value::Nil => Value::Bool(false),
            _ => return Err(mismatch()),
        },
        "matches" | "contains" | "startsWith" | "endsWith" => {
            let (Value::String(text), Value::String(other)) = (left, right) else {
                return Err(mismatch());
            };
            Value::Bool(match operator {
                "contains" => text.contains(other.as_str()),
                "startsWith" => text.starts_with(other.as_str()),
                "endsWith" => text.ends_with(other.as_str()),
                _ => regex::Regex::new(other)
                    .map_err(|error| ExprError::new(error.to_string()))?
                    .is_match(text),
            })
        }
        _ => return Err(ExprError::new(format!("unknown operator {operator}"))),
    })
}

/// expr's `int()`, which Argo also offers as `asInt`.
pub fn to_int(value: &Value) -> Result<Value> {
    match value {
        Value::Int(number) => Ok(Value::Int(*number)),
        Value::Float(number) => Ok(Value::Int(*number as i64)),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| ExprError::new(format!("invalid operation: int({text})"))),
        other => Err(ExprError::new(format!(
            "invalid operation: int({})",
            other.type_name()
        ))),
    }
}

/// expr's `float()`, which Argo also offers as `asFloat`.
pub fn to_float(value: &Value) -> Result<Value> {
    match value {
        Value::Int(number) => Ok(Value::Float(*number as f64)),
        Value::Float(number) => Ok(Value::Float(*number)),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| ExprError::new(format!("invalid operation: float({text})"))),
        other => Err(ExprError::new(format!(
            "invalid operation: float({})",
            other.type_name()
        ))),
    }
}

fn text_argument<'a>(name: &str, values: &'a [Value], index: usize) -> Result<&'a str> {
    match values.get(index) {
        Some(Value::String(text)) => Ok(text),
        Some(other) => Err(ExprError::new(format!(
            "{name}: argument {} must be a string, not {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(ExprError::new(format!(
            "{name}: missing argument {}",
            index + 1
        ))),
    }
}

/// The builtins of expr and the helpers Argo adds to the environment.
fn builtin(name: &str, values: Vec<Value>) -> Result<Value> {
    let first = values.first().cloned().unwrap_or(Value::Nil);
    let text = |index: usize| text_argument(name, &values, index);
    Ok(match name {
        "asInt" | "int" => to_int(&first)?,
        "asFloat" | "float" => to_float(&first)?,
        "string" => Value::String(first.to_go_string()),
        "toJson" => Value::String(first.to_json()),
        "toJSON" => Value::String(first.to_json_indented()),
        "fromJSON" => Value::parse_json(text(0)?).map_err(ExprError::new)?,
        "jsonpath" => {
            let document = Value::parse_json(text(0)?).map_err(ExprError::new)?;
            jsonpath::get(text(1)?, &document).map_err(ExprError::new)?
        }
        "len" => Value::Int(match &first {
            Value::String(text) => text.chars().count(),
            Value::Array(items) => items.len(),
            Value::Map(map) => map.len(),
            other => {
                return Err(ExprError::new(format!(
                    "invalid argument for len (type {})",
                    other.type_name()
                )));
            }
        } as i64),
        "type" => Value::string(first.type_name()),
        "abs" => match first {
            Value::Int(number) => Value::Int(number.wrapping_abs()),
            Value::Float(number) => Value::Float(number.abs()),
            other => {
                return Err(ExprError::new(format!(
                    "invalid argument for abs ({})",
                    other.type_name()
                )));
            }
        },
        "ceil" | "floor" | "round" => {
            let number = number(&first)
                .ok_or_else(|| ExprError::new(format!("invalid argument for {name}")))?;
            Value::Float(match name {
                "ceil" => number.ceil(),
                "floor" => number.floor(),
                _ => number.round(),
            })
        }
        "trim" => match values.get(1) {
            Some(Value::String(cutset)) => {
                Value::string(text(0)?.trim_matches(|c| cutset.contains(c)))
            }
            _ => Value::string(text(0)?.trim()),
        },
        "trimPrefix" => {
            let prefix = match values.get(1) {
                Some(Value::String(prefix)) => prefix.as_str(),
                _ => " ",
            };
            Value::string(text(0)?.strip_prefix(prefix).unwrap_or(text(0)?))
        }
        "trimSuffix" => {
            let suffix = match values.get(1) {
                Some(Value::String(suffix)) => suffix.as_str(),
                _ => " ",
            };
            Value::string(text(0)?.strip_suffix(suffix).unwrap_or(text(0)?))
        }
        "upper" => Value::string(text(0)?.to_uppercase()),
        "lower" => Value::string(text(0)?.to_lowercase()),
        "split" => Value::Array(text(0)?.split(text(1)?).map(Value::string).collect()),
        "splitAfter" => Value::Array(
            text(0)?
                .split_inclusive(text(1)?)
                .map(Value::string)
                .collect(),
        ),
        "replace" => Value::string(text(0)?.replace(text(1)?, text(2)?)),
        "repeat" => Value::string(text(0)?.repeat(
            usize::try_from(integer(values.get(1).unwrap_or(&Value::Int(0)))?).unwrap_or(0),
        )),
        "indexOf" => Value::Int(text(0)?.find(text(1)?).map_or(-1, |index| {
            text(0).unwrap_or_default()[..index].chars().count() as i64
        })),
        "lastIndexOf" => Value::Int(text(0)?.rfind(text(1)?).map_or(-1, |index| {
            text(0).unwrap_or_default()[..index].chars().count() as i64
        })),
        "hasPrefix" => Value::Bool(text(0)?.starts_with(text(1)?)),
        "hasSuffix" => Value::Bool(text(0)?.ends_with(text(1)?)),
        "join" => {
            let separator = match values.get(1) {
                Some(Value::String(separator)) => separator.as_str(),
                _ => "",
            };
            Value::string(
                array(&first, name)?
                    .iter()
                    .map(Value::to_go_string)
                    .collect::<Vec<_>>()
                    .join(separator),
            )
        }
        "max" | "min" => {
            let items: Vec<Value> = match values.as_slice() {
                [Value::Array(items)] => items.clone(),
                _ => values.clone(),
            };
            let mut best: Option<Value> = None;
            for item in items {
                best = Some(match best {
                    None => item,
                    Some(current) => {
                        let order = compare(&item, &current)?;
                        let better = if name == "max" {
                            order == std::cmp::Ordering::Greater
                        } else {
                            order == std::cmp::Ordering::Less
                        };
                        if better { item } else { current }
                    }
                });
            }
            best.unwrap_or(Value::Nil)
        }
        "mean" | "median" => {
            let mut numbers: Vec<f64> = array(&first, name)?
                .iter()
                .map(|item| {
                    number(item).ok_or_else(|| ExprError::new(format!("{name} needs numbers")))
                })
                .collect::<Result<_>>()?;
            if numbers.is_empty() {
                Value::Float(0.0)
            } else if name == "mean" {
                Value::Float(numbers.iter().sum::<f64>() / numbers.len() as f64)
            } else {
                numbers.sort_by(|a, b| a.total_cmp(b));
                let middle = numbers.len() / 2;
                Value::Float(if numbers.len().is_multiple_of(2) {
                    (numbers[middle - 1] + numbers[middle]) / 2.0
                } else {
                    numbers[middle]
                })
            }
        }
        "toBase64" => Value::string(sprig::base64_encode(text(0)?.as_bytes())),
        "fromBase64" => Value::string(
            String::from_utf8_lossy(&sprig::base64_decode(text(0)?).map_err(ExprError::new)?)
                .into_owned(),
        ),
        "first" => array(&first, name)?.first().cloned().unwrap_or(Value::Nil),
        "last" => array(&first, name)?.last().cloned().unwrap_or(Value::Nil),
        "get" => match (&first, values.get(1)) {
            (Value::Array(_) | Value::Map(_) | Value::String(_), Some(key)) => {
                member(&first, key, true).unwrap_or(Value::Nil)
            }
            _ => Value::Nil,
        },
        "keys" | "values" => match &first {
            Value::Map(map) if name == "keys" => {
                Value::Array(map.keys().cloned().map(Value::String).collect())
            }
            Value::Map(map) => Value::Array(map.values().cloned().collect()),
            other => {
                return Err(ExprError::new(format!(
                    "{name}: expected a map, got {}",
                    other.type_name()
                )));
            }
        },
        "reverse" => Value::Array(array(&first, name)?.iter().rev().cloned().collect()),
        "uniq" => {
            let mut unique: Vec<Value> = Vec::new();
            for item in array(&first, name)? {
                if !unique.iter().any(|seen| equal(seen, item)) {
                    unique.push(item.clone());
                }
            }
            Value::Array(unique)
        }
        "flatten" => {
            fn flatten(items: &[Value], out: &mut Vec<Value>) {
                for item in items {
                    match item {
                        Value::Array(inner) => flatten(inner, out),
                        other => out.push(other.clone()),
                    }
                }
            }
            let mut out = Vec::new();
            flatten(array(&first, name)?, &mut out);
            Value::Array(out)
        }
        "concat" => {
            let mut out = Vec::new();
            for value in &values {
                out.extend(array(value, name)?.iter().cloned());
            }
            Value::Array(out)
        }
        "sort" => {
            let mut items = array(&first, name)?.to_vec();
            let mut failure = None;
            items.sort_by(|a, b| {
                compare(a, b).unwrap_or_else(|error| {
                    failure = Some(error);
                    std::cmp::Ordering::Equal
                })
            });
            if let Some(error) = failure {
                return Err(error);
            }
            if matches!(values.get(1), Some(Value::String(order)) if order == "desc") {
                items.reverse();
            }
            Value::Array(items)
        }
        "take" => {
            let count =
                usize::try_from(integer(values.get(1).unwrap_or(&Value::Int(0)))?).unwrap_or(0);
            Value::Array(array(&first, name)?.iter().take(count).cloned().collect())
        }
        "toPairs" => match &first {
            Value::Map(map) => Value::Array(
                map.iter()
                    .map(|(key, value)| {
                        Value::Array(vec![Value::string(key.clone()), value.clone()])
                    })
                    .collect(),
            ),
            _ => return Err(ExprError::new("toPairs: expected a map")),
        },
        "fromPairs" => {
            let mut map = BTreeMap::new();
            for pair in array(&first, name)? {
                if let Value::Array(pair) = pair
                    && let [key, value] = pair.as_slice()
                {
                    map.insert(key.to_go_string(), value.clone());
                }
            }
            Value::Map(map)
        }
        _ => return Err(ExprError::new(format!("unknown func {name}"))),
    })
}

#[cfg(test)]
#[path = "expr_tests.rs"]
mod tests;
