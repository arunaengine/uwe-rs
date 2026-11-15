//! Builds the tree of a Cheetah template from its tokens, and turns Cheetah expressions into
//! Python for python-evaluator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Error;
use crate::lexer::{self, Token};

/// The name of the builtin that does Cheetah's NameMapper lookup in translated expressions.
pub const FOLLOW: &str = "__cheetah_follow__";

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(String),
    /// A Python expression whose value is printed.
    Value {
        expression: String,
        line: usize,
    },
    /// `#if`, `#elif` and `#else`: the first branch whose Python condition holds renders.
    If {
        branches: Vec<(String, Vec<Node>)>,
        otherwise: Vec<Node>,
        line: usize,
    },
    /// `#set`: a Python assignment statement.
    Set {
        statement: String,
        line: usize,
    },
}

/// Parses a template. Directives beyond tier 1, like `#for`, fail with their line.
pub fn parse(text: &str) -> Result<Vec<Node>, Error> {
    let tokens = lexer::tokens(text)?;
    let mut at = 0;
    let nodes = block(&tokens, &mut at, None)?;
    if let Some(token) = tokens.get(at) {
        return Err(Error::template(
            line_of(token),
            "a directive outside any block".to_owned(),
        ));
    }
    Ok(nodes)
}

/// The nodes up to the `#end`, `#elif` or `#else` that closes `inside`, which is left at `at`.
fn block(tokens: &[Token], at: &mut usize, inside: Option<&str>) -> Result<Vec<Node>, Error> {
    let mut nodes = Vec::new();
    while let Some(token) = tokens.get(*at) {
        match token {
            Token::Text(text) => nodes.push(Node::Text(text.clone())),
            Token::Placeholder { expression, line } => nodes.push(Node::Value {
                expression: python(expression, *line)?,
                line: *line,
            }),
            Token::Directive {
                name,
                argument,
                line,
                ..
            } => match name.as_str() {
                "end" | "elif" | "else" => {
                    if inside.is_none() {
                        return Err(Error::template(*line, format!("#{name} without #if")));
                    }
                    return Ok(nodes);
                }
                "if" => {
                    *at += 1;
                    nodes.push(conditional(tokens, at, argument, *line)?);
                }
                "set" => nodes.push(Node::Set {
                    statement: assignment(argument, *line)?,
                    line: *line,
                }),
                "raw" => {
                    *at += 1;
                    let body = block(tokens, at, Some("raw"))?;
                    nodes.extend(body);
                    expect_end(tokens, *at, "raw", *line)?;
                }
                "pass" => {}
                other => {
                    return Err(Error::template(
                        *line,
                        format!("the directive #{other} is not supported yet"),
                    ));
                }
            },
        }
        *at += 1;
    }
    match inside {
        Some(name) => Err(Error::template(
            tokens.last().map_or(1, line_of),
            format!("#{name} without its #end {name}"),
        )),
        None => Ok(nodes),
    }
}

/// An `#if` and its `#elif` and `#else` branches; `at` is after the `#if` and ends on its end.
fn conditional(
    tokens: &[Token],
    at: &mut usize,
    condition: &str,
    line: usize,
) -> Result<Node, Error> {
    let mut branches = vec![(python(condition, line)?, block(tokens, at, Some("if"))?)];
    let mut otherwise = Vec::new();
    let mut closed = false;
    loop {
        // A short `#if a: b` closes at its line end; an `#elif` or `#else` right after reopens it.
        let next = match (tokens.get(*at), tokens.get(*at + 1)) {
            (
                Some(Token::Directive {
                    name, short: true, ..
                }),
                Some(Token::Directive { name: after, .. }),
            ) if name == "end" && (after == "elif" || after == "else") => {
                *at += 1;
                &tokens[*at]
            }
            (Some(token), _) => token,
            (None, _) => return Err(Error::template(line, "#if without its #end if".to_owned())),
        };
        let Token::Directive {
            name,
            argument,
            line: at_line,
            ..
        } = next
        else {
            unreachable!("block stops only at directives");
        };
        match name.as_str() {
            "elif" if !closed => {
                *at += 1;
                let body = block(tokens, at, Some("if"))?;
                branches.push((python(argument, *at_line)?, body));
            }
            "else" if !closed => {
                *at += 1;
                otherwise = block(tokens, at, Some("if"))?;
                closed = true;
            }
            "end" => {
                expect_end(tokens, *at, "if", line)?;
                return Ok(Node::If {
                    branches,
                    otherwise,
                    line,
                });
            }
            _ => return Err(Error::template(*at_line, format!("#{name} after #else"))),
        }
    }
}

fn expect_end(tokens: &[Token], at: usize, name: &str, line: usize) -> Result<(), Error> {
    match tokens.get(at) {
        Some(Token::Directive {
            name: end,
            argument,
            ..
        }) if end == "end" && argument == name => Ok(()),
        Some(Token::Directive { argument, line, .. }) => Err(Error::template(
            *line,
            format!("#end {argument} closes #{name}"),
        )),
        _ => Err(Error::template(
            line,
            format!("#{name} without its #end {name}"),
        )),
    }
}

fn line_of(token: &Token) -> usize {
    match token {
        Token::Text(_) => 1,
        Token::Placeholder { line, .. } | Token::Directive { line, .. } => *line,
    }
}

/// The Python statement of a `#set` argument: `$x = ...`, also with `global` or `+=`.
fn assignment(argument: &str, line: usize) -> Result<String, Error> {
    let argument = argument.strip_prefix("global ").unwrap_or(argument).trim();
    let target_end = lexer::identifier_end(argument, usize::from(argument.starts_with('$')));
    let target = argument[..target_end].trim_start_matches('$');
    let rest = argument[target_end..].trim_start();
    let operator_end = rest.find('=').map(|at| at + 1).filter(|at| {
        !rest[*at..].starts_with('=') && rest[..*at].chars().all(|c| "+-*/%&|^<>=".contains(c))
    });
    match (target.is_empty(), operator_end) {
        (false, Some(end)) => Ok(format!(
            "{target} {} {}",
            &rest[..end],
            python(&rest[end..], line)?
        )),
        _ => Err(Error::template(
            line,
            format!("#set {argument} is not an assignment"),
        )),
    }
}

/// Turns a Cheetah expression into Python: `$a.b.c` becomes a NameMapper lookup that tries
/// dict keys, then attributes, and calls methods left without parentheses. Strings stay as
/// they are: Cheetah does not fill placeholders inside Python strings of directives.
pub fn python(expression: &str, line: usize) -> Result<String, Error> {
    let mut out = String::new();
    let mut at = 0;
    let text = expression.trim();
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().unwrap_or(' ');
        if c == '\'' || c == '"' {
            let end = lexer::string_end(text, at)
                .ok_or_else(|| Error::template(line, format!("an unclosed string in {text}")))?;
            out.push_str(&text[at..end]);
            at = end;
        } else if c == '_' || c.is_alphabetic() {
            let end = lexer::identifier_end(text, at);
            out.push_str(&text[at..end]);
            at = end;
        } else if c == '$' && rest[1..].starts_with(|c: char| c == '_' || c.is_alphabetic()) {
            let first_end = lexer::identifier_end(text, at + 1);
            let mut end = first_end;
            while text[end..].starts_with('.')
                && text[end + 1..].starts_with(|c: char| c == '_' || c.is_alphabetic())
            {
                end = lexer::identifier_end(text, end + 1);
            }
            let path = text[first_end..end].trim_start_matches('.');
            let autocall = if text[end..].starts_with('(') {
                "False"
            } else {
                "True"
            };
            out.push_str(&format!(
                "{FOLLOW}({}, {path:?}, {autocall})",
                &text[at + 1..first_end]
            ));
            at = end;
        } else if c == '$' {
            return Err(Error::template(
                line,
                format!("a $ that starts no placeholder in {text}"),
            ));
        } else {
            out.push(c);
            at += c.len_utf8();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn follow(name: &str, path: &str) -> String {
        format!("{FOLLOW}({name}, {path:?}, True)")
    }

    #[test]
    fn branches_and_values() {
        let nodes = parse("#if $a:\nA $v\n#elif $b:\nB\n#else\nC\n#end if\n").expect("parses");
        let expected = Node::If {
            branches: vec![
                (
                    follow("a", ""),
                    vec![
                        Node::Text("A ".to_owned()),
                        Node::Value {
                            expression: follow("v", ""),
                            line: 2,
                        },
                        Node::Text("\n".to_owned()),
                    ],
                ),
                (follow("b", ""), vec![Node::Text("B\n".to_owned())]),
            ],
            otherwise: vec![Node::Text("C\n".to_owned())],
            line: 1,
        };
        assert_eq!(nodes, [expected]);
        let short = parse("#if $a: A\n#else: B\n").expect("parses");
        let Node::If {
            branches,
            otherwise,
            ..
        } = &short[0]
        else {
            panic!("an if: {short:?}");
        };
        assert_eq!(branches[0].1, [Node::Text("A\n".to_owned())]);
        assert_eq!(*otherwise, [Node::Text("B\n".to_owned())]);
    }

    #[test]
    fn translates_names() {
        let python = python("$x.y.z == 'a$b' and len($l) > 0 and $f.g(1)[0]", 1).expect("python");
        let expected = format!(
            "{} == 'a$b' and len({}) > 0 and {FOLLOW}(f, \"g\", False)(1)[0]",
            follow("x", "y.z"),
            follow("l", "")
        );
        assert_eq!(python, expected, "strings keep their $");
        let set = parse("#set $n = $m + 1\n").expect("parses");
        let statement = format!("n = {} + 1", follow("m", ""));
        assert_eq!(set, [Node::Set { statement, line: 1 }]);
    }

    #[test]
    fn refuses_later_tiers() {
        let error = parse("a\n#for $i in $x:\n$i\n#end for\n").expect_err("a loop");
        let text = error.to_string();
        assert!(text.contains("line 2") && text.contains("#for"), "{text}");
        assert!(parse("#if $a:\nA\n").is_err(), "an if without its end");
        assert!(parse("#else\n").is_err(), "an else without an if");
    }
}
