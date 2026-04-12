//! `when` conditions: govaluate (github.com/Knetic/govaluate v3) after simple-tag substitution,
//! with Argo's rule that a bare word is a string, so `heads == heads` holds. A word with a dot,
//! which govaluate reads as an accessor, stays an error, as in Argo.
//!
//! One rule is iunctus's own: a missing operand, as a tag that substitution left empty makes
//! it (` == heads`), reads as an empty string, and ordering comparisons with it are false.
//! govaluate fails on such conditions; the rule lets runs whose outputs are empty, like mock
//! runs, decide their conditions instead of failing.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::value::go_float;

type Result<T> = std::result::Result<T, String>;

/// Decides a substituted `when` condition. An empty condition holds.
pub fn should_execute(when: &str) -> Result<bool> {
    if when.is_empty() {
        return Ok(true);
    }
    let tokens = lex(when).map_err(|error| invalid(when, &error))?;
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    let expression = parser.ternary().map_err(|error| invalid(when, &error))?;
    if parser.position < parser.tokens.len() {
        return Err(invalid(
            when,
            &format!("unexpected {:?}", parser.tokens[parser.position]),
        ));
    }
    match evaluate(&expression)
        .map_err(|error| format!("failed to evaluate 'when' result '{when}': {error}"))?
    {
        Operand::Bool(flag) => Ok(flag),
        other => Err(format!(
            "expected boolean evaluation for '{when}', got {}",
            other.text()
        )),
    }
}

fn invalid(when: &str, error: &str) -> String {
    format!("invalid 'when' expression '{when}': {error}")
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Bool(bool),
    /// A word with a dot, which govaluate reads as a parameter accessor.
    Accessor(String),
    Op(String),
    Open,
    Close,
    Comma,
}

const SYMBOLS: [&str; 24] = [
    "==", "!=", ">=", "<=", "=~", "!~", "&&", "||", "**", ">>", "<<", "??", ">", "<", "+", "-",
    "*", "/", "%", "&", "|", "^", "!", "~",
];

fn lex(text: &str) -> Result<Vec<Token>> {
    let characters: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
        } else if character.is_ascii_digit() || character == '.' {
            let start = index;
            if characters[index..].starts_with(&['0', 'x']) {
                index += 2;
                while index < characters.len() && characters[index].is_ascii_hexdigit() {
                    index += 1;
                }
                let digits: String = characters[start + 2..index].iter().collect();
                let number = u64::from_str_radix(&digits, 16)
                    .map_err(|_| format!("invalid hex number 0x{digits}"))?;
                tokens.push(Token::Number(number as f64));
                continue;
            }
            while index < characters.len()
                && (characters[index].is_ascii_digit() || characters[index] == '.')
            {
                index += 1;
            }
            let digits: String = characters[start..index].iter().collect();
            let number = digits
                .parse::<f64>()
                .map_err(|_| format!("unable to parse numeric value '{digits}' to float64"))?;
            tokens.push(Token::Number(number));
        } else if character == '"' || character == '\'' {
            let mut text = String::new();
            index += 1;
            loop {
                let Some(&next) = characters.get(index) else {
                    return Err("unclosed string literal".into());
                };
                index += 1;
                if next == '\\' && index < characters.len() {
                    text.push(characters[index]);
                    index += 1;
                } else if next == character {
                    break;
                } else {
                    text.push(next);
                }
            }
            tokens.push(Token::Str(text));
        } else if character == '[' {
            let end = characters[index..]
                .iter()
                .position(|c| *c == ']')
                .ok_or("unclosed parameter bracket")?;
            // An escaped parameter name is a variable, which Argo turns into a string.
            tokens.push(Token::Str(
                characters[index + 1..index + end].iter().collect(),
            ));
            index += end + 1;
        } else if character.is_alphabetic() {
            let start = index;
            while index < characters.len()
                && (characters[index].is_alphanumeric()
                    || characters[index] == '_'
                    || characters[index] == '.')
            {
                index += 1;
            }
            let word: String = characters[start..index].iter().collect();
            tokens.push(match word.as_str() {
                "true" => Token::Bool(true),
                "false" => Token::Bool(false),
                "in" | "IN" => Token::Op("in".into()),
                _ if word.contains('.') => {
                    if word.ends_with('.') {
                        return Err(format!("hanging accessor on token '{word}'"));
                    }
                    Token::Accessor(word)
                }
                _ => Token::Str(word),
            });
        } else if character == '(' {
            tokens.push(Token::Open);
            index += 1;
        } else if character == ')' {
            tokens.push(Token::Close);
            index += 1;
        } else if character == ',' {
            tokens.push(Token::Comma);
            index += 1;
        } else if character == '?' || character == ':' {
            if characters[index..].starts_with(&['?', '?']) {
                tokens.push(Token::Op("??".into()));
                index += 2;
            } else {
                tokens.push(Token::Op(character.to_string()));
                index += 1;
            }
        } else {
            // govaluate reads a run of symbols as one operator.
            let start = index;
            while index < characters.len() && is_symbol(characters[index]) {
                index += 1;
            }
            let symbol: String = characters[start..index.max(start + 1)].iter().collect();
            index = index.max(start + 1);
            if !SYMBOLS.contains(&symbol.as_str()) {
                return Err(format!("invalid token: '{symbol}'"));
            }
            tokens.push(Token::Op(symbol));
        }
    }
    Ok(tokens)
}

fn is_symbol(character: char) -> bool {
    !(character.is_alphanumeric()
        || character.is_whitespace()
        || matches!(
            character,
            '_' | '(' | ')' | '[' | ']' | '"' | '\'' | ',' | '?' | ':' | '.'
        ))
}

#[derive(Clone, Debug, PartialEq)]
enum Expression {
    Value(Operand),
    Accessor(String),
    List(Vec<Expression>),
    Prefix(String, Box<Expression>),
    Binary(String, Box<Expression>, Box<Expression>),
    Ternary(Box<Expression>, Box<Expression>, Box<Expression>),
}

#[derive(Clone, Debug, PartialEq)]
enum Operand {
    Bool(bool),
    Number(f64),
    Str(String),
    List(Vec<Operand>),
    /// An operand that substitution left empty.
    Missing,
}

impl Operand {
    fn text(&self) -> String {
        match self {
            Operand::Bool(flag) => flag.to_string(),
            Operand::Number(number) => go_float(*number),
            Operand::Str(text) => text.clone(),
            Operand::List(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Operand::text)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Operand::Missing => String::new(),
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn peek_op(&self) -> Option<&str> {
        match self.tokens.get(self.position) {
            Some(Token::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn eat(&mut self, operators: &[&str]) -> Option<String> {
        let op = self.peek_op()?.to_owned();
        if operators.contains(&op.as_str()) {
            self.position += 1;
            Some(op)
        } else {
            None
        }
    }

    fn ternary(&mut self) -> Result<Expression> {
        let condition = self.binary(0)?;
        if self.eat(&["?"]).is_some() {
            let then = self.ternary()?;
            let otherwise = if self.eat(&[":"]).is_some() {
                self.ternary()?
            } else {
                Expression::Value(Operand::Missing)
            };
            return Ok(Expression::Ternary(
                Box::new(condition),
                Box::new(then),
                Box::new(otherwise),
            ));
        }
        Ok(condition)
    }

    fn binary(&mut self, level: usize) -> Result<Expression> {
        const LEVELS: [&[&str]; 8] = [
            &["??"],
            &["||"],
            &["&&"],
            &["==", "!=", ">", ">=", "<", "<=", "=~", "!~", "in"],
            &["&", "|", "^"],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.exponent();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(op) = self.eat(LEVELS[level]) {
            let right = self.binary(level + 1)?;
            left = Expression::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn exponent(&mut self) -> Result<Expression> {
        let base = self.prefix()?;
        if self.eat(&["**"]).is_some() {
            let power = self.exponent()?;
            return Ok(Expression::Binary(
                "**".into(),
                Box::new(base),
                Box::new(power),
            ));
        }
        Ok(base)
    }

    fn prefix(&mut self) -> Result<Expression> {
        if let Some(op) = self.eat(&["!", "-", "~"]) {
            let operand = self.prefix()?;
            return Ok(Expression::Prefix(op, Box::new(operand)));
        }
        self.value()
    }

    fn value(&mut self) -> Result<Expression> {
        let Some(token) = self.tokens.get(self.position).cloned() else {
            return Ok(Expression::Value(Operand::Missing));
        };
        Ok(match token {
            Token::Number(number) => {
                self.position += 1;
                Expression::Value(Operand::Number(number))
            }
            Token::Str(text) => {
                self.position += 1;
                Expression::Value(Operand::Str(text))
            }
            Token::Bool(flag) => {
                self.position += 1;
                Expression::Value(Operand::Bool(flag))
            }
            Token::Accessor(name) => {
                self.position += 1;
                Expression::Accessor(name)
            }
            Token::Open => {
                self.position += 1;
                let mut items = vec![self.ternary()?];
                while self.tokens.get(self.position) == Some(&Token::Comma) {
                    self.position += 1;
                    items.push(self.ternary()?);
                }
                if self.tokens.get(self.position) != Some(&Token::Close) {
                    return Err("unbalanced parenthesis".into());
                }
                self.position += 1;
                if items.len() == 1 {
                    items.remove(0)
                } else {
                    Expression::List(items)
                }
            }
            // The operand before an operator or at the end is missing.
            Token::Op(_) | Token::Close | Token::Comma => Expression::Value(Operand::Missing),
        })
    }
}

fn evaluate(expression: &Expression) -> Result<Operand> {
    Ok(match expression {
        Expression::Value(value) => value.clone(),
        Expression::Accessor(name) => {
            return Err(format!(
                "no parameter '{}' found",
                name.split('.').next().unwrap_or(name)
            ));
        }
        Expression::List(items) => {
            Operand::List(items.iter().map(evaluate).collect::<Result<_>>()?)
        }
        Expression::Prefix(op, operand) => match (op.as_str(), evaluate(operand)?) {
            ("!", Operand::Bool(flag)) => Operand::Bool(!flag),
            ("-", Operand::Number(number)) => Operand::Number(-number),
            ("~", Operand::Number(number)) => Operand::Number(!(number as i64) as f64),
            (op, value) => {
                return Err(format!(
                    "value '{}' cannot be used with the prefix '{op}'",
                    value.text()
                ));
            }
        },
        Expression::Ternary(condition, then, otherwise) => match evaluate(condition)? {
            Operand::Bool(true) => evaluate(then)?,
            Operand::Bool(false) => evaluate(otherwise)?,
            other => {
                return Err(format!(
                    "value '{}' cannot be used with the ternary operator",
                    other.text()
                ));
            }
        },
        Expression::Binary(op, left, right) => binary(op, left, right)?,
    })
}
