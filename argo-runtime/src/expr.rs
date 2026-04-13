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
