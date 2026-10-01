//! Groovy's `ConfigSlurper`, which old pipelines use in `lib/` to read their own config files,
//! like nf-core/hicar reads the `publishDir` of a process from `conf/modules.config`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::ast::{self, ConfigStatement};
use nextflow_parser::{Span, parse_config};

use super::super::{Error, Interpreter, unsupported};
use crate::lower::lower_template_value;
use crate::lowered::Module;
use crate::value::{Closure, Key, Map, MapKind, Value};

impl Interpreter<'_> {
    /// Parses config text into maps, including blocks and named or patterned selectors.
    /// ConfigObject creates empty maps for missing keys and names, so absent params print `[:]`.
    pub(super) fn slurp(&mut self, text: &str, span: Span) -> Result<Value, Error> {
        let parsed = parse_config(text);
        if !parsed.errors.is_empty() {
            return Err(unsupported("a ConfigSlurper text with errors", span));
        }
        let source = "template = { -> def auto = null\nauto = { key -> [:].withDefault(auto) }\n\
                      [:].withDefault(auto) }.call()";
        let unknown = self.template_value(source, "the names of a ConfigSlurper text", span)?;
        let tree = match &unknown {
            Value::Map(map) => new_map(map.borrow().default.clone()),
            _ => new_map(None),
        };
        self.slurp_statements(&parsed.tree.statements, &tree, &unknown, text, span)?;
        Ok(tree)
    }

    fn slurp_statements(
        &mut self,
        statements: &[ConfigStatement],
        tree: &Value,
        unknown: &Value,
        text: &str,
        span: Span,
    ) -> Result<(), Error> {
        for statement in statements {
            match statement {
                ConfigStatement::Assign { path, value, .. } => {
                    let value = self.slurp_value(value, unknown, text, span)?;
                    let names: Vec<&str> = path.iter().map(|name| name.text.as_str()).collect();
                    let (last, parents) = names.split_last().expect("a name");
                    let parent = parents
                        .iter()
                        .fold(tree.clone(), |tree, name| child(&tree, name));
                    if let Value::Map(map) = parent {
                        map.borrow_mut()
                            .entries
                            .insert(Key(Value::string(*last)), value);
                    }
                }
                ConfigStatement::Block { name, body, .. } => {
                    let block = child(tree, &name.text);
                    self.slurp_statements(body, &block, unknown, text, span)?;
                }
                ConfigStatement::Selector { target, body, .. } => {
                    let block = child(tree, &target.text);
                    self.slurp_statements(body, &block, unknown, text, span)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The value of a setting, whose closures keep reading the unknown names as `params`.
    fn slurp_value(
        &mut self,
        value: &ast::Expression,
        unknown: &Value,
        text: &str,
        span: Span,
    ) -> Result<Value, Error> {
        let name = |text: &str| ast::Name {
            text: text.into(),
            span: value.span,
        };
        let closure = ast::Expression {
            kind: ast::ExpressionKind::Closure(Box::new(ast::Closure {
                parameters: Some(vec![ast::Parameter {
                    name: name("params"),
                    annotation: None,
                    default: None,
                }]),
                body: ast::Block {
                    statements: vec![ast::Statement {
                        kind: ast::StatementKind::Expression(value.clone()),
                        span: value.span,
                    }],
                    span: value.span,
                },
            })),
            span: value.span,
        };
        let what = "a value of a ConfigSlurper text";
        let code = lower_template_value(&closure, text).map_err(|errors| {
            let first = errors.first().map_or("", |error| error.message.as_str());
            unsupported(format!("{what}: {first}"), span)
        })?;
        let Value::Closure(closure) = self.evaluate(&Rc::new(code), &Rc::new(Module::default()))?
        else {
            return Err(unsupported(what, span));
        };
        self.call_closure_value(&closure, vec![unknown.clone()], span)
    }
}

/// An empty map, which makes a value of a key it lacks with `default`.
fn new_map(default: Option<Rc<Closure>>) -> Value {
    Value::Map(Rc::new(RefCell::new(Map {
        kind: MapKind::LinkedHashMap,
        entries: Default::default(),
        default,
    })))
}

/// The map of a name in a map, made when it is missing.
fn child(tree: &Value, name: &str) -> Value {
    let Value::Map(map) = tree else {
        return new_map(None);
    };
    let key = Key(Value::string(name));
    if let Some(existing @ Value::Map(_)) = map.borrow().entries.get(&key) {
        return existing.clone();
    }
    let child = new_map(map.borrow().default.clone());
    map.borrow_mut().entries.insert(key, child.clone());
    child
}
