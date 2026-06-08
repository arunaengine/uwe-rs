//! Templates, as Groovy's `SimpleTemplateEngine` and the process `template` function render
//! them: text with `$name.path` and `${expression}` parts, whose names come from a binding.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::ast::ConfigStatement;
use nextflow_parser::{Span, parse_config};

use super::{Error, Interpreter, unsupported};
use crate::lower::lower_template_value;
use crate::lowered::Module;
use crate::value::{PathValue, Value};

impl Interpreter<'_> {
    /// Renders a template. Names come from `binding` first, then from the globals.
    pub fn render_template(
        &mut self,
        text: &str,
        binding: Vec<(Rc<str>, Rc<RefCell<Value>>)>,
        span: Span,
    ) -> Result<String, Error> {
        let previous = std::mem::replace(&mut self.task_variables, binding);
        let result = self.render_parts(text, span);
        self.task_variables = previous;
        result
    }

    /// `template 'name'` in a process script: `templates/name` of the module, else of the
    /// project, rendered with the task's variables.
    pub(super) fn template_file(
        &mut self,
        module: &Rc<Module>,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, Error> {
        let [name] = arguments else {
            return Err(unsupported("`template` with these arguments", span));
        };
        let name = name.to_string();
        let mut candidates = Vec::new();
        if name.starts_with('/') {
            candidates.push(PathValue::parse(&name, "/"));
        }
        if let Some(directory) = module.directory.borrow().as_deref() {
            candidates.push(PathValue::parse(
                &format!("{directory}/templates/{name}"),
                "/",
            ));
        }
        if let Some(Value::Path(project)) = self.globals.get("projectDir") {
            candidates.push(project.resolve(&format!("templates/{name}")));
        }
        let mut found = None;
        for candidate in candidates {
            if self
                .host
                .metadata(&candidate)
                .map_err(|e| e.or_at(span))?
                .is_some()
            {
                found = Some(candidate);
                break;
            }
        }
        let Some(path) = found else {
            let message = format!("Process template file does not exist: {name}");
            return Err(super::thrown(
                crate::exception::Exception::new("NoSuchFileException", message),
                span,
            ));
        };
        let bytes = self.host.read(&path).map_err(|e| e.or_at(span))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let binding = self.task_variables.clone();
        Ok(Value::string(self.render_template(&text, binding, span)?))
    }

    fn render_parts(&mut self, text: &str, span: Span) -> Result<String, Error> {
        if text.contains("<%") {
            return self.render_code(text, span);
        }
        let mut result = String::new();
        let mut rest = text;
        while let Some(dollar) = rest.find('$') {
            // `\$` is a plain dollar sign.
            if rest[..dollar].ends_with('\\') {
                result.push_str(&rest[..dollar - 1]);
                result.push('$');
                rest = &rest[dollar + 1..];
                continue;
            }
            result.push_str(&rest[..dollar]);
            let after = &rest[dollar + 1..];
            let (expression, length) = if let Some(inner) = after.strip_prefix('{') {
                let end = closing_brace(inner)
                    .ok_or_else(|| unsupported("a template with an unclosed `${`", span))?;
                (&inner[..end], end + 2)
            } else {
                let length = dotted_name(after);
                if length == 0 {
                    result.push('$');
                    rest = after;
                    continue;
                }
                (&after[..length], length)
            };
            let value = self.template_expression(expression, span)?;
            result.push_str(&value.to_interpolated());
            rest = &after[length..];
        }
        result.push_str(rest);
        Ok(result)
    }

    /// A template with `<% code %>` or `<%= expression %>`, which Groovy's template engines
    /// turn into code: each text part goes to `out` as a GString, and the code runs in place.
    fn render_code(&mut self, text: &str, span: Span) -> Result<String, Error> {
        let mut source = String::from("template = { ->\ndef out = []\n");
        let mut rest = text;
        while let Some(start) = rest.find("<%") {
            push_text(&mut source, &rest[..start]);
            let after = &rest[start + 2..];
            let end = after
                .find("%>")
                .ok_or_else(|| unsupported("a template with an unclosed `<%`", span))?;
            match after[..end].strip_prefix('=') {
                Some(expression) => source.push_str(&format!("out << ({expression})\n")),
                None => {
                    source.push_str(&after[..end]);
                    source.push('\n');
                }
            }
            rest = &after[end + 2..];
        }
        push_text(&mut source, rest);
        source.push_str("out.join('')\n}\n");
        let what = "the code of this template";
        let Value::Closure(closure) = self.template_value(&source, what, span)? else {
            return Err(unsupported(what, span));
        };
        let result = self.call_closure_value(&closure, Vec::new(), span)?;
        Ok(result.to_string())
    }

    fn template_expression(&mut self, expression: &str, span: Span) -> Result<Value, Error> {
        let what = format!("the template expression `{expression}`");
        self.template_value(&format!("template = {expression}"), &what, span)
    }

    /// The value of `template = <value>`, whose names come from the binding. `what` names the
    /// part of the template for errors.
    fn template_value(&mut self, source: &str, what: &str, span: Span) -> Result<Value, Error> {
        let parsed = parse_config(source);
        let value = match parsed.tree.statements.first() {
            Some(ConfigStatement::Assign { value, .. }) if parsed.errors.is_empty() => value,
            _ => return Err(unsupported(what, span)),
        };
        let code = lower_template_value(value, source).map_err(|_| unsupported(what, span))?;
        self.evaluate(&Rc::new(code), &Rc::new(Module::default()))
    }
}

/// Adds `out << """text"""` for a text part of a template, with `"` escaped, as Groovy's
/// `SimpleTemplateEngine` writes it.
fn push_text(source: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    source.push_str("out << \"\"\"");
    for character in text.chars() {
        if character == '"' {
            source.push('\\');
        }
        source.push(character);
    }
    source.push_str("\"\"\"\n");
}

/// Where the `}` that closes a `${` is: braces nest, and braces in quotes do not count.
fn closing_brace(text: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    for (index, character) in text.char_indices() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(character),
            (None, '{') => depth += 1,
            (None, '}') if depth == 0 => return Some(index),
            (None, '}') => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The length of a name with properties, like `workflow.manifest.name`, as a GString takes it.
fn dotted_name(text: &str) -> usize {
    let mut length = 0;
    let mut expect_name = true;
    for (index, character) in text.char_indices() {
        let valid = if expect_name {
            character.is_alphabetic() || character == '_'
        } else {
            character.is_alphanumeric() || character == '_'
        };
        if valid {
            expect_name = false;
            length = index + character.len_utf8();
        } else if character == '.' && !expect_name {
            expect_name = true;
        } else {
            break;
        }
    }
    length
}
