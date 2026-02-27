//! Tests expression parsing: precedence, line breaks, calls, closures, collections and strings.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::ast::{
    Access, Argument, BinaryOperator, Expression, ExpressionKind, GStringPart, Literal, MapKey,
    Statement, StatementKind, Type, TypeArgument, UnaryOperator,
};
use crate::parser::Parser;
use crate::token::{Dialect, TokenKind};

/// Parses one expression and renders it; the whole input must be consumed.
pub(in crate::parser) fn parse(source: &str) -> String {
    let mut parser = Parser::new(source, Dialect::Script);
    let parsed = parser.expression();
    assert!(parser.errors.is_empty(), "{source}: {:?}", parser.errors);
    let expression = parsed.unwrap_or_else(|_| panic!("{source}: no expression"));
    assert_eq!(
        parser.peek(),
        TokenKind::EndOfFile,
        "{source}: input left over"
    );
    render(&expression)
}

fn fails(source: &str) -> bool {
    let mut parser = Parser::new(source, Dialect::Script);
    let failed = parser.expression().is_err() || !parser.errors.is_empty();
    failed || parser.peek() != TokenKind::EndOfFile
}

pub(in crate::parser) fn render(expression: &Expression) -> String {
    match &expression.kind {
        ExpressionKind::Variable(name) => name.clone(),
        ExpressionKind::Literal(literal) => match literal {
            Literal::Integer(text) | Literal::Float(text) => text.clone(),
            Literal::String(text) => format!("'{text}'"),
            Literal::Boolean(value) => value.to_string(),
            Literal::Null => "null".to_owned(),
        },
        ExpressionKind::GString(parts) => {
            let parts: String = parts
                .iter()
                .map(|part| match part {
                    GStringPart::Text(text) => text.clone(),
                    GStringPart::Value(value) => format!("${{{}}}", render(value)),
                })
                .collect();
            format!("\"{parts}\"")
        }
        ExpressionKind::New { class, arguments } => {
            format!(
                "(new {}{})",
                render_type(class),
                render_arguments(arguments)
            )
        }
        ExpressionKind::Parenthesized(inner) => format!("(paren {})", render(inner)),
        ExpressionKind::Closure(closure) => {
            let parameters = match &closure.parameters {
                None => String::new(),
                Some(parameters) => {
                    let names: Vec<_> = parameters.iter().map(|p| p.name.text.clone()).collect();
                    format!("{} -> ", names.join(","))
                }
            };
            let body: Vec<_> = closure
                .body
                .statements
                .iter()
                .map(render_statement)
                .collect();
            format!("{{{parameters}{}}}", body.join("; "))
        }
        ExpressionKind::List(items) => {
            let items: Vec<_> = items.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        ExpressionKind::Map(entries) if entries.is_empty() => "[:]".to_owned(),
        ExpressionKind::Map(entries) => {
            let entries: Vec<_> = entries
                .iter()
                .map(|entry| match &entry.key {
                    MapKey::Name(name) => format!("{}: {}", name.text, render(&entry.value)),
                    MapKey::Expression(key) => {
                        format!("({}): {}", render(key), render(&entry.value))
                    }
                })
                .collect();
            format!("[{}]", entries.join(", "))
        }
        ExpressionKind::PrimitiveType(name) => name.clone(),
        ExpressionKind::Property {
            target,
            name,
            access,
        } => format!(
            "({} {} {})",
            access_text(*access),
            render(target),
            name.text
        ),
        ExpressionKind::MethodCall {
            target,
            name,
            access,
            arguments,
            parentheses,
        } => {
            let head = match target {
                None => name.text.clone(),
                Some(target) => format!("{}{} {}", access_text(*access), name.text, render(target)),
            };
            let mark = if *parentheses { "" } else { "cmd " };
            format!("({mark}{head}{})", render_arguments(arguments))
        }
        ExpressionKind::Call { callee, arguments } => {
            format!("(call {}{})", render(callee), render_arguments(arguments))
        }
        ExpressionKind::Index { target, index } => {
            let index: Vec<_> = index.iter().map(render).collect();
            format!("(index {} {})", render(target), index.join(" "))
        }
        ExpressionKind::Unary { operator, operand } => {
            let symbol = match operator {
                UnaryOperator::Not => "!",
                UnaryOperator::BitNot => "~",
                UnaryOperator::Plus => "+",
                UnaryOperator::Minus => "-",
            };
            format!("({symbol} {})", render(operand))
        }
        ExpressionKind::Binary {
            operator,
            left,
            right,
        } => format!(
            "({} {} {})",
            binary_text(*operator),
            render(left),
            render(right)
        ),
        ExpressionKind::Cast {
            operand,
            target_type,
        } => format!("(as {} {})", render(operand), render_type(target_type)),
        ExpressionKind::InstanceOf {
            operand,
            target_type,
            negated,
        } => {
            let keyword = if *negated {
                "!instanceof"
            } else {
                "instanceof"
            };
            format!(
                "({keyword} {} {})",
                render(operand),
                render_type(target_type)
            )
        }
        ExpressionKind::Ternary {
            condition,
            then,
            otherwise,
        } => format!(
            "(? {} {} {})",
            render(condition),
            render(then),
            render(otherwise)
        ),
        ExpressionKind::Elvis { value, fallback } => {
            format!("(?: {} {})", render(value), render(fallback))
        }
    }
}

pub(in crate::parser) fn render_statement(statement: &Statement) -> String {
    match &statement.kind {
        StatementKind::Expression(expression) => render(expression),
        StatementKind::Assign {
            target,
            operator,
            value,
        } => format!("(set{:?} {} {})", operator, render(target), render(value)),
        StatementKind::MultipleAssign { names, value } => {
            let names: Vec<_> = names.iter().map(|name| name.text.clone()).collect();
            format!("(set ({}) {})", names.join(" "), render(value))
        }
        StatementKind::Declaration {
            names,
            legacy_type,
            initializer,
            destructure,
        } => {
            let names: Vec<_> = names
                .iter()
                .map(|typed| match &typed.annotation {
                    Some(annotation) => format!("{}: {}", typed.name.text, render_type(annotation)),
                    None => typed.name.text.clone(),
                })
                .collect();
            let names = if *destructure {
                format!("({})", names.join(" "))
            } else {
                names.join(" ")
            };
            let legacy = legacy_type.as_ref().map(|t| format!("{} ", render_type(t)));
            let value = initializer.as_ref().map(|v| format!(" {}", render(v)));
            format!(
                "(def {}{names}{})",
                legacy.unwrap_or_default(),
                value.unwrap_or_default()
            )
        }
        StatementKind::If {
            condition,
            then,
            otherwise,
        } => {
            let block = |block: &crate::ast::Block| {
                let body: Vec<_> = block.statements.iter().map(render_statement).collect();
                format!("{{{}}}", body.join("; "))
            };
            let otherwise = otherwise.as_ref().map(|b| format!(" {}", block(b)));
            format!(
                "(if {} {}{})",
                render(condition),
                block(then),
                otherwise.unwrap_or_default()
            )
        }
        StatementKind::Try { body, catches } => {
            let catches: Vec<_> = catches
                .iter()
                .map(|catch| {
                    let types: Vec<_> = catch
                        .types
                        .iter()
                        .map(|name| {
                            name.parts
                                .iter()
                                .map(|p| p.text.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .collect();
                    format!(" (catch {} {})", types.join("|"), catch.name.text)
                })
                .collect();
            format!("(try {}{})", body.statements.len(), catches.concat())
        }
        StatementKind::Return(value) => match value {
            Some(value) => format!("(return {})", render(value)),
            None => "(return)".to_owned(),
        },
        StatementKind::Throw(value) => format!("(throw {})", render(value)),
        StatementKind::Assert { condition, message } => match message {
            Some(message) => format!("(assert {} {})", render(condition), render(message)),
            None => format!("(assert {})", render(condition)),
        },
        StatementKind::Labeled { label, statement } => {
            format!("{}: {}", label.text, render_statement(statement))
        }
        StatementKind::Empty => "(empty)".to_owned(),
    }
}

fn render_arguments(arguments: &[Argument]) -> String {
    arguments
        .iter()
        .map(|argument| match &argument.name {
            Some(name) => format!(" {}={}", name.text, render(&argument.value)),
            None => format!(" {}", render(&argument.value)),
        })
        .collect()
}

fn render_type(annotation: &Type) -> String {
    let name: Vec<_> = annotation
        .name
        .parts
        .iter()
        .map(|p| p.text.as_str())
        .collect();
    let mut text = name.join(".");
    if !annotation.arguments.is_empty() {
        let arguments: Vec<_> = annotation
            .arguments
            .iter()
            .map(|argument| match argument {
                TypeArgument::Type(inner) => render_type(inner),
                TypeArgument::Wildcard => "?".to_owned(),
            })
            .collect();
        text = format!("{text}<{}>", arguments.join(","));
    }
    if annotation.nullable {
        text.push('?');
    }
    text + &"[]".repeat(annotation.dimensions as usize)
}

fn access_text(access: Access) -> &'static str {
    match access {
        Access::Normal => ".",
        Access::Safe => "?.",
        Access::Spread => "*.",
    }
}

fn binary_text(operator: BinaryOperator) -> &'static str {
    use BinaryOperator::*;
    match operator {
        Power => "**",
        Multiply => "*",
        Divide => "/",
        Modulo => "%",
        Add => "+",
        Subtract => "-",
        LeftShift => "<<",
        RightShift => ">>",
        UnsignedShift => ">>>",
        RangeInclusive => "..",
        RangeExclusiveRight => "..<",
        Less => "<",
        LessEqual => "<=",
        Greater => ">",
        GreaterEqual => ">=",
        In => "in",
        NotIn => "!in",
        Equal => "==",
        NotEqual => "!=",
        Spaceship => "<=>",
        RegexFind => "=~",
        RegexMatch => "==~",
        BitAnd => "&",
        Xor => "^",
        BitOr => "|",
        And => "&&",
        Or => "||",
    }
}

#[test]
fn operator_precedence() {
    assert_eq!(parse("a + b * c"), "(+ a (* b c))");
    assert_eq!(parse("-a ** b"), "(- (** a b))");
    assert_eq!(parse("!a == b"), "(== (! a) b)");
    assert_eq!(parse("a || b && c | d"), "(|| a (&& b (| c d)))");
    assert_eq!(parse("a - b - c"), "(- (- a b) c)");
    assert_eq!(
        parse("a =~ /x/ && b ==~ 'y'"),
        "(&& (=~ a 'x') (==~ b 'y'))"
    );
    assert_eq!(
        parse("x in [1, 2] || y !in z"),
        "(|| (in x [1, 2]) (!in y z))"
    );
}

#[test]
fn relational_levels() {
    assert_eq!(parse("a >= b as int"), "(>= a (as b int))");
    assert_eq!(parse("a < b instanceof C"), "(< a (instanceof b C))");
    assert_eq!(parse("a in b !instanceof C"), "(in a (!instanceof b C))");
    assert_eq!(parse("a instanceof B as C"), "(as (instanceof a B) C)");
    assert_eq!(parse("a as B >= c"), "(>= (as a B) c)");
    assert_eq!(parse("a << b as C"), "(as (<< a b) C)");
}

#[test]
fn conditional_associativity() {
    assert_eq!(parse("a ? b : c ? d : e"), "(? a b (? c d e))");
    assert_eq!(parse("a ?: b ?: c"), "(?: a (?: b c))");
    assert_eq!(parse("a\n    ? b\n    : c"), "(? a b c)");
}

#[test]
fn newlines_before_operators() {
    assert_eq!(parse("a *\n b"), "(* a b)");
    assert_eq!(parse("a\n * b"), "(* a b)");
    assert_eq!(parse("a +\n b"), "(+ a b)");
    assert!(fails("a\n + b"));
    assert_eq!(
        parse("ch\n    .map { it }\n    .view()"),
        "(.view (cmd .map ch {it}))"
    );
}

#[test]
fn shifts_and_ranges() {
    assert_eq!(parse("a << b"), "(<< a b)");
    assert_eq!(parse("a >> b >>> c"), "(>>> (>> a b) c)");
    assert_eq!(parse("1..3"), "(.. 1 3)");
    assert_eq!(parse("0..<n"), "(..< 0 n)");
}

#[test]
fn calls_and_closures() {
    assert_eq!(parse("foo(a, b)"), "(foo a b)");
    assert_eq!(parse("foo(x) { it }"), "(foo x {it})");
    assert_eq!(parse("foo { it }"), "(cmd foo {it})");
    assert_eq!(parse("x.map { a, b -> a }"), "(cmd .map x {a,b -> a})");
    assert_eq!(parse("f()(1)"), "(call (f) 1)");
    assert_eq!(parse("foo(a: 1, 'b': 2, c)"), "(foo a=1 b=2 c)");
    assert_eq!(parse("foo(\n  a,\n  b,\n)"), "(foo a b)");
    assert_eq!(parse("{ -> 1 }"), "{ -> 1}");
    assert_eq!(parse("{ String s, int n = 2 -> s }"), "{s,n -> s}");
}

#[test]
fn closure_after_parentheses() {
    assert_eq!(parse("(a.b) { it }"), "(cmd .b a {it})");
    assert_eq!(parse("((a)) { it }"), "(cmd a {it})");
    assert_eq!(parse("(f(x)) { it }"), "(f x {it})");
    assert_eq!(parse("(a.b)(1)"), "(call (paren (. a b)) 1)");
}

#[test]
fn constant_method_names() {
    assert_eq!(parse("'m'(1)"), "(m 1)");
    assert_eq!(parse("\"m\" { it }"), "(cmd m {it})");
    assert_eq!(parse("/m/()"), "(m)");
    assert_eq!(parse("int()"), "(int)");
    assert_eq!(parse("\"$m\"()"), "(call \"${m}\")");
}

#[test]
fn property_access_kinds() {
    assert_eq!(parse("a.b?.c*.d"), "(*. (?. (. a b) c) d)");
    assert_eq!(parse("meta.id"), "(. meta id)");
    assert_eq!(parse("x.'**'"), "(. x **)");
    assert_eq!(
        parse("task.ext.args ?: ''"),
        "(?: (. (. task ext) args) '')"
    );
    assert_eq!(parse("2.GB"), "(. 2 GB)");
    assert_eq!(parse("a[0][1, 2]"), "(index (index a 0) 1 2)");
}

#[test]
fn lists_and_maps() {
    assert_eq!(parse("[]"), "[]");
    assert_eq!(parse("[:]"), "[:]");
    assert_eq!(parse("[1, 2,]"), "[1, 2]");
    assert_eq!(parse("[\n  1,\n  2\n]"), "[1, 2]");
    assert_eq!(
        parse("[a: 1, (b): 2, 'c': 3, in: 4]"),
        "[a: 1, ((paren b)): 2, ('c'): 3, in: 4]"
    );
    assert_eq!(parse("[meta, reads]"), "[meta, reads]");
    assert_eq!(parse("[\n  \"a\": 1,\n  b: 2\n]"), "[('a'): 1, b: 2]");
}

#[test]
fn string_forms() {
    assert_eq!(parse(r"'a\tbA'"), "'a\tbA'");
    assert_eq!(parse("\"\"\"\n  x\n\"\"\""), "'\n  x\n'");
    assert_eq!(parse(r"/a\/b\d/"), r"'a/b\d'");
    assert_eq!(
        parse(r#""a $x.y ${z + 1}\$""#),
        "\"a ${(. x y)} ${(+ z 1)}$\""
    );
    assert_eq!(parse(r#""${meta.id}.bam""#), "\"${(. meta id)}.bam\"");
}

#[test]
fn types_and_objects() {
    assert_eq!(parse("x as Integer"), "(as x Integer)");
    assert_eq!(parse("x instanceof List"), "(instanceof x List)");
    assert_eq!(
        parse("x !instanceof java.util.Map"),
        "(!instanceof x java.util.Map)"
    );
    assert_eq!(
        parse("x instanceof List ? a : b"),
        "(? (instanceof x List) a b)"
    );
    assert_eq!(
        parse("c ? x instanceof List ? 'l' : 's' : ''"),
        "(? c (? (instanceof x List) 'l' 's') '')"
    );
    assert_eq!(parse("new File('x')"), "(new File 'x')");
    assert_eq!(parse("new ArrayList<String>()"), "(new ArrayList<String>)");
}

#[test]
fn unclosed_type_arguments() {
    assert_eq!(parse("a as B < c"), "(< (as a B) c)");
    assert_eq!(
        parse("a as List<String>=~b"),
        "(>= (< (as a List) String) (~ b))"
    );
    assert_eq!(
        parse("a instanceof List<String>"),
        "(instanceof a List<String>)"
    );
}

#[test]
fn nullable_or_conditional() {
    assert_eq!(parse("a instanceof Path?+b"), "(+ (instanceof a Path?) b)");
    assert_eq!(parse("a as List?\n  b : c"), "(? (as a List) b c)");
    assert_eq!(parse("a as String? ?: b"), "(?: (as a String?) b)");
}

#[test]
fn rejects_incomplete_expressions() {
    assert!(fails("a +"));
    assert!(fails("foo(a"));
    assert!(fails("[a: ]"));
    assert!(fails("\"${}\""));
}

#[test]
fn rejects_duplicate_names() {
    assert!(fails("foo(a: 1, 'a': 2)"));
    assert!(fails("new Foo(in: 1, x, in: 2)"));
    assert!(!fails("foo(a: 1, b: 2, [a: 3])"));
}
