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
