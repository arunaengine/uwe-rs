//! DSL1 scripts, which Nextflow ran before 22.12: each process reads its inputs `from`
//! channels and writes its outputs `into` channels where the script defines it, and the
//! statements of the script run in order around the processes. Lowering turns such a script
//! into the dataflow model of DSL2: the processes lose `from` and `into`, and an entry workflow
//! runs the statements and calls each process where the script defined it (decision 14).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;
use nextflow_parser::ast::{
    self, Access, Argument, Block, Expression, ExpressionKind as Syntax, Item, Literal, Name,
    NamedOutput, ProcessInput, Statement, StatementKind as Kind,
};

/// Whether a script is DSL1: `nextflow.enable.dsl = 1`, or a script without workflows with a
/// process that reads `from` a channel or writes `into` one.
pub(super) fn is_dsl1(script: &ast::Script) -> bool {
    let flag = script.items.iter().any(|item| {
        matches!(item, Item::FeatureFlag { name, value, .. }
            if name.iter().map(|part| part.text.as_str()).eq(["enable", "dsl"])
                && matches!(&value.kind, Syntax::Literal(Literal::Integer(text)) if text == "1"))
    });
    let workflows = script
        .items
        .iter()
        .any(|item| matches!(item, Item::Workflow(_)));
    let chained = script.items.iter().any(|item| match item {
        Item::Process(process) => is_dsl1_process(process),
        // A process inside a block of the script is DSL1 too.
        Item::Statement(statement) => has_process(statement),
        _ => false,
    });
    flag || (!workflows && chained)
}

/// Whether a process reads `from` a channel or writes `into` one.
fn is_dsl1_process(process: &ast::Process) -> bool {
    let inputs = process.inputs.iter().filter_map(|input| match input {
        ProcessInput::Statement(statement) => Some(statement),
        _ => None,
    });
    let outputs = process.outputs.iter().filter_map(|output| match output {
        NamedOutput::Statement(statement) => Some(statement),
        NamedOutput::Named { .. } => None,
    });
    inputs
        .chain(outputs)
        .any(|statement| chain_call(statement, &["from", "into"]).is_some())
}

/// Whether a statement holds a process, like one inside an `if`.
fn has_process(statement: &Statement) -> bool {
    match &statement.kind {
        Kind::Process(_) => true,
        Kind::If {
            then, otherwise, ..
        } => (then.statements.iter())
            .chain(otherwise.iter().flat_map(|block| &block.statements))
            .any(has_process),
        _ => false,
    }
}

/// A DSL1 script as a DSL2 script: the same items, with processes that have no `from` and
/// `into`, and an entry workflow with the statements of the script and a call of each process
/// where the script defines it. The names that the calls give their outputs, as `into` does,
/// come with it.
pub(super) fn convert(script: &ast::Script) -> (ast::Script, Vec<String>) {
    let mut items = Vec::new();
    let mut main = Vec::new();
    let mut channels = Vec::new();
    for item in &script.items {
        match item {
            Item::Statement(statement) => {
                let statement = super::script_assignment(statement);
                main.extend(convert_statement(statement, &mut items, &mut channels));
            }
            Item::Process(process) => {
                main.extend(define(process, &mut items, &mut channels));
            }
            other => items.push(other.clone()),
        }
    }
    let span = match (main.first(), main.last()) {
        (Some(first), Some(last)) => first.span.to(last.span),
        _ => Span::default(),
    };
    items.push(Item::Workflow(Box::new(ast::Workflow {
        name: None,
        takes: Vec::new(),
        main: Block {
            statements: main,
            span,
        },
        emits: Vec::new(),
        publishers: Vec::new(),
        on_complete: None,
        on_error: None,
        span,
    })));
    (ast::Script { items }, channels)
}

/// A statement of the script, with each process inside it, like one in an `if`, defined as an
/// item and called where it was.
fn convert_statement(
    statement: Statement,
    items: &mut Vec<Item>,
    channels: &mut Vec<String>,
) -> Vec<Statement> {
    let span = statement.span;
    match statement.kind {
        Kind::Process(process) => define(&process, items, channels),
        Kind::If {
            condition,
            then,
            otherwise,
        } => {
            let block = |block: Block, items: &mut Vec<Item>, channels: &mut Vec<String>| {
                let statements = (block.statements.into_iter())
                    .flat_map(|statement| convert_statement(statement, items, channels))
                    .collect();
                Block {
                    statements,
                    span: block.span,
                }
            };
            let then = block(then, items, channels);
            let otherwise = otherwise.map(|otherwise| block(otherwise, items, channels));
            vec![Statement {
                kind: Kind::If {
                    condition,
                    then,
                    otherwise,
                },
                span,
            }]
        }
        kind => vec![Statement { kind, span }],
    }
}

/// Defines a process as an item, and gives the statements that call it.
fn define(
    process: &ast::Process,
    items: &mut Vec<Item>,
    channels: &mut Vec<String>,
) -> Vec<Statement> {
    let (process, sources, targets) = split_process(process);
    let calls = calls(&process.name, sources, &targets, process.span);
    channels.extend(targets.into_iter().flatten().map(|name| name.text));
    items.push(Item::Process(Box::new(process)));
    calls
}

/// A process without `from` and `into`: its inputs and outputs in DSL2 form, the source of
/// each input and the channel names of each output.
fn split_process(process: &ast::Process) -> (ast::Process, Vec<Expression>, Vec<Vec<Name>>) {
    let mut process = process.clone();
    let mut sources = Vec::new();
    for input in &mut process.inputs {
        let ProcessInput::Statement(statement) = input else {
            continue;
        };
        if let Some((qualifier, arguments)) = chain_call(statement, &["from"]) {
            if let Some(source) = arguments.first() {
                sources.push(source.value.clone());
            }
            *statement = qualifier_statement(qualifier, statement.span);
        }
    }
    let mut targets = Vec::new();
    for output in &mut process.outputs {
        let NamedOutput::Statement(statement) = output else {
            targets.push(Vec::new());
            continue;
        };
        let mut names = Vec::new();
        if let Some((qualifier, arguments)) = chain_call(statement, &["into"]) {
            names = arguments
                .iter()
                .filter_map(|argument| match &argument.value.kind {
                    Syntax::Variable(name) => Some(Name {
                        text: name.clone(),
                        span: argument.value.span,
                    }),
                    _ => None,
                })
                .collect();
            *statement = qualifier_statement(qualifier, statement.span);
        } else {
            *statement = qualifier_statement(expression_of(statement).clone(), statement.span);
        }
        targets.push(names);
    }
    (process, sources, targets)
}

/// The call `target.name(arguments)` that a statement is, for one of the names.
fn chain_call(statement: &Statement, names: &[&str]) -> Option<(Expression, Vec<Argument>)> {
    let Kind::Expression(expression) = &statement.kind else {
        return None;
    };
    match &expression.kind {
        Syntax::MethodCall {
            target: Some(target),
            name,
            arguments,
            parentheses: false,
            ..
        } if names.contains(&name.text.as_str()) => Some(((**target).clone(), arguments.clone())),
        _ => None,
    }
}

fn expression_of(statement: &Statement) -> &Expression {
    match &statement.kind {
        Kind::Expression(expression) => expression,
        _ => unreachable!("only expression statements are chains"),
    }
}

/// A DSL1 qualifier as DSL2 writes it: `set` is `tuple`, a bare name in a tuple is a `val`, and
/// `optional true` of the command chain is the option `optional: true`.
fn qualifier_statement(qualifier: Expression, span: Span) -> Statement {
    let expression = dsl2_qualifier(qualifier);
    Statement {
        kind: Kind::Expression(expression),
        span,
    }
}

fn dsl2_qualifier(qualifier: Expression) -> Expression {
    let span = qualifier.span;
    match qualifier.kind {
        // `... optional true` or `... type('dir')` calls a method on the qualifier, which DSL2
        // gives as an option.
        Syntax::MethodCall {
            target: Some(target),
            name,
            arguments,
            ..
        } => {
            let mut inner = dsl2_qualifier(*target);
            if let Syntax::MethodCall {
                arguments: inner_arguments,
                ..
            } = &mut inner.kind
            {
                let value = arguments.into_iter().next().map_or(
                    Expression {
                        kind: Syntax::Literal(Literal::Boolean(true)),
                        span: name.span,
                    },
                    |argument| argument.value,
                );
                inner_arguments.push(Argument {
                    name: Some(name),
                    value,
                });
            }
            inner
        }
        Syntax::MethodCall {
            target: None,
            name,
            access,
            arguments,
            parentheses,
        } if matches!(name.text.as_str(), "set" | "tuple") => {
            let arguments = arguments
                .into_iter()
                .map(|argument| match &argument.value.kind {
                    Syntax::Variable(variable)
                        if argument.name.is_none() && variable != "stdin" =>
                    {
                        Argument {
                            name: None,
                            value: call("val", vec![argument.value.clone()], argument.value.span),
                        }
                    }
                    _ => argument,
                })
                .collect();
            Expression {
                kind: Syntax::MethodCall {
                    target: None,
                    name: Name {
                        text: "tuple".into(),
                        span: name.span,
                    },
                    access,
                    arguments,
                    parentheses,
                },
                span,
            }
        }
        kind => Expression { kind, span },
    }
}

/// The call of a process where a DSL1 script defines it, and the names of its outputs:
/// `P(a, b)` and `ch = P.out[0]` for `into ch`.
fn calls(
    process: &Name,
    sources: Vec<Expression>,
    targets: &[Vec<Name>],
    span: Span,
) -> Vec<Statement> {
    let mut statements = vec![Statement {
        kind: Kind::Expression(call(&process.text, sources, span)),
        span,
    }];
    for (index, names) in targets.iter().enumerate() {
        for name in names {
            let out = Expression {
                kind: Syntax::Property {
                    target: Box::new(variable(&process.text, span)),
                    name: Name {
                        text: "out".into(),
                        span,
                    },
                    access: Access::Normal,
                },
                span,
            };
            let value = Expression {
                kind: Syntax::Index {
                    target: Box::new(out),
                    index: vec![Expression {
                        kind: Syntax::Literal(Literal::Integer(index.to_string())),
                        span,
                    }],
                },
                span,
            };
            statements.push(Statement {
                kind: Kind::Assign {
                    target: variable(&name.text, name.span),
                    operator: ast::AssignOperator::Assign,
                    value,
                },
                span,
            });
        }
    }
    statements
}

fn call(name: &str, arguments: Vec<Expression>, span: Span) -> Expression {
    Expression {
        kind: Syntax::MethodCall {
            target: None,
            name: Name {
                text: name.into(),
                span,
            },
            access: Access::Normal,
            arguments: arguments
                .into_iter()
                .map(|value| Argument { name: None, value })
                .collect(),
            parentheses: true,
        },
        span,
    }
}

fn variable(name: &str, span: Span) -> Expression {
    Expression {
        kind: Syntax::Variable(name.into()),
        span,
    }
}
