//! The Groovy expressions of nf-cmgg worksheets: the allowed subset, its check and evaluation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::value::Value;
use nextflow_parser::ast::{
    Argument, Closure, Expression, ExpressionKind, GStringPart, Item, MapKey, StatementKind,
};
use nextflow_parser::parse_script;

/// The only methods that an expression may call.
const ALLOWED: &[&str] = &[
    "toLowerCase",
    "toUpperCase",
    "trim",
    "strip",
    "contains",
    "startsWith",
    "endsWith",
    "matches",
    "replace",
    "replaceAll",
    "substring",
    "size",
    "length",
    "isEmpty",
    "toLong",
    "toInteger",
    "toDouble",
    "toFloat",
    "toBoolean",
    "toBigDecimal",
    "toString",
    "equals",
    "compareTo",
    "abs",
    "getAt",
    "get",
    "asBoolean",
    "asType",
];

/// A checked expression, as a function of `data` and `params`.
#[derive(Debug)]
pub(super) struct Code {
    module: Rc<Module>,
}

impl Code {
    /// Checks and compiles an expression; the error reads like Groovy's compiler message.
    pub(super) fn compile(text: &str) -> Result<Code, String> {
        let source = format!("def f(data, params) {{\n{text}\n}}\n");
        let parsed = parse_script(&source);
        if let Some(error) = parsed.errors.first() {
            return Err(format!("startup failed:\n{}\n\n1 error\n", error.message));
        }
        let mut problems = Vec::new();
        for item in &parsed.tree.items {
            let Item::Function(function) = item else {
                problems.push("only one expression is allowed".to_owned());
                continue;
            };
            for statement in &function.body.statements {
                match &statement.kind {
                    StatementKind::Expression(expression) => {
                        check(expression, &source, &mut problems);
                    }
                    _ => problems.push("only expressions are allowed".to_owned()),
                }
            }
        }
        if let Some(problem) = problems.first() {
            return Err(format!(
                "startup failed:\nGeneral error during semantic analysis: {problem}\n\n1 error\n"
            ));
        }
        let module = lower_module(&parsed.tree, &source).map_err(|errors| {
            let messages: Vec<String> = errors.iter().map(|error| error.message.clone()).collect();
            format!("startup failed:\n{}\n", messages.join("\n"))
        })?;
        Ok(Code {
            module: Rc::new(module),
        })
    }

    /// The value of the expression for a sample's `data` and the run's `params`.
    pub(super) fn evaluate(&self, data: &Value, params: &Value) -> Result<Value, String> {
        let mut host = Quiet;
        let mut interpreter = Interpreter::new(&mut host);
        let arguments = vec![data.clone(), params.clone()];
        let result = interpreter.call_function(&self.module, "f", arguments);
        result.map_err(|error| match error.exception() {
            Some(exception) => exception.message.clone(),
            None => error.to_string(),
        })
    }
}

/// A host that expressions cannot print to.
struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

/// Collects the constructs that the plugin's secure compiler rejects, in Groovy's words.
fn check(expression: &Expression, source: &str, problems: &mut Vec<String>) {
    let shown = || {
        let span = expression.span;
        let text = source
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default();
        text.replace(['"', '\''], "")
    };
    match &expression.kind {
        ExpressionKind::New { arguments, .. } => {
            problems.push(format!(
                "Expression [ConstructorCallExpression] is not allowed: {}",
                shown()
            ));
            arguments_of(arguments, source, problems);
        }
        ExpressionKind::MethodCall {
            target,
            name,
            arguments,
            ..
        } => {
            let static_call = target.as_deref().is_some_and(|target| {
                matches!(&target.kind, ExpressionKind::Variable(name) if name.starts_with(char::is_uppercase))
            });
            if static_call {
                problems.push(format!(
                    "Expression [StaticMethodCallExpression] is not allowed: {}",
                    shown()
                ));
            } else if !ALLOWED.contains(&name.text.as_str()) {
                problems.push(format!(
                    "Expression [MethodCallExpression] is not allowed: {}",
                    shown()
                ));
            }
            if let Some(target) = target {
                check(target, source, problems);
            }
            arguments_of(arguments, source, problems);
        }
        ExpressionKind::Call { callee, arguments } => {
            problems.push(format!(
                "Expression [MethodCallExpression] is not allowed: {}",
                shown()
            ));
            check(callee, source, problems);
            arguments_of(arguments, source, problems);
        }
        ExpressionKind::Closure(closure) => closure_of(closure, source, problems),
        ExpressionKind::GString(parts) => {
            for part in parts {
                if let GStringPart::Value(value) = part {
                    check(value, source, problems);
                }
            }
        }
        ExpressionKind::List(items) => {
            for item in items {
                check(item, source, problems);
            }
        }
        ExpressionKind::Map(entries) => {
            for entry in entries {
                if let MapKey::Expression(key) = &entry.key {
                    check(key, source, problems);
                }
                check(&entry.value, source, problems);
            }
        }
        ExpressionKind::Parenthesized(inner)
        | ExpressionKind::Spread(inner)
        | ExpressionKind::Property { target: inner, .. }
        | ExpressionKind::Unary { operand: inner, .. }
        | ExpressionKind::Cast { operand: inner, .. }
        | ExpressionKind::InstanceOf { operand: inner, .. }
        | ExpressionKind::Postfix { operand: inner, .. } => check(inner, source, problems),
        ExpressionKind::Index { target, index } => {
            check(target, source, problems);
            for part in index {
                check(part, source, problems);
            }
        }
        ExpressionKind::Binary { left, right, .. }
        | ExpressionKind::Elvis {
            value: left,
            fallback: right,
        }
        | ExpressionKind::DynamicProperty {
            target: left,
            name: right,
        }
        | ExpressionKind::Assign {
            target: left,
            value: right,
            ..
        } => {
            check(left, source, problems);
            check(right, source, problems);
        }
        ExpressionKind::Ternary {
            condition,
            then,
            otherwise,
        } => {
            check(condition, source, problems);
            check(then, source, problems);
            check(otherwise, source, problems);
        }
        ExpressionKind::Variable(_)
        | ExpressionKind::Literal(_)
        | ExpressionKind::PrimitiveType(_) => {}
    }
}

fn arguments_of(arguments: &[Argument], source: &str, problems: &mut Vec<String>) {
    for argument in arguments {
        check(&argument.value, source, problems);
    }
}

fn closure_of(closure: &Closure, source: &str, problems: &mut Vec<String>) {
    for statement in &closure.body.statements {
        match &statement.kind {
            StatementKind::Expression(expression) => check(expression, source, problems),
            _ => problems.push("only expressions are allowed".to_owned()),
        }
    }
}

/// The names that an expression reads as `<prefix>.<name>`, like `data.tag`, in order.
pub(super) fn references(text: &str, prefix: &str) -> Vec<String> {
    let lead = format!("{prefix}.");
    let mut names = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while let Some(found) = rest.find(&lead) {
        let at = offset + found;
        let before = text[..at].chars().next_back();
        let after = &text[at + lead.len()..];
        let name: String = (after.chars())
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && !name.is_empty()
            && !names.contains(&name)
        {
            names.push(name);
        }
        offset = at + lead.len();
        rest = &text[offset..];
    }
    names
}

#[cfg(test)]
mod tests {
    use nextflow_evaluator::value::Value;

    use super::{Code, references};

    #[test]
    fn rejects_other_methods() {
        let error = Code::compile("data.sample.split(\"x\")").unwrap_err();
        assert!(
            error
                .contains("Expression [MethodCallExpression] is not allowed: data.sample.split(x)"),
            "{error}"
        );
        assert!(Code::compile("new File('x')").is_err());
        assert!(Code::compile("data.kind?.toLowerCase() == 'dna'").is_ok());
    }

    #[test]
    fn evaluates_with_data() {
        let code = Code::compile("data.tag + '_' + data.kind?.toLowerCase()").unwrap();
        let data = Value::map([(Value::string("tag"), Value::string("none"))]);
        let value = code.evaluate(&data, &Value::map([])).unwrap();
        assert_eq!(value.to_string(), "none_null");
    }

    #[test]
    fn finds_references() {
        let text = "input.sample + data.sample + mydata.x + data.sample";
        assert_eq!(references(text, "data"), ["sample"]);
        assert_eq!(references(text, "input"), ["sample"]);
    }
}
