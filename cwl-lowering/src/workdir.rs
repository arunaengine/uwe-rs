//! The entries of the working folder of a task from CWL `InitialWorkDirRequirement`, with an
//! expr for each part that code computes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Expression, Listing, ListingEntry, Part};
use wir::{Content, FileName, Staged, Type};

use crate::tool::Computed;

/// The code of an expression for an expr, or why it has none.
pub type Code<'a> = dyn Fn(&Expression) -> Result<String, String> + 'a;

/// The entries of a listing. A part with code fills a task input, `entry.<n>` or
/// `entryname.<n>`, through an expr of `values`.
pub fn entries(
    listing: &Listing,
    inputs: &[&str],
    values: &mut Vec<Computed>,
    code: &Code<'_>,
) -> Result<Vec<Staged>, String> {
    let mut staged = Vec::new();
    let mut fill = |port: String, expression: &Expression, r#type: Option<Type>| {
        let text = code(expression)?;
        values.push(Computed {
            port: port.clone(),
            code: text,
            this: None,
            r#type,
        });
        Ok::<_, String>(port)
    };
    let entries = match listing {
        Listing::Expression(expression) => {
            let port = fill("entry.0".into(), expression, Some(Type::Dynamic))?;
            return Ok(vec![entry(Content::Input { input: port }, None, false)]);
        }
        Listing::Entries(entries) => entries,
    };
    for (index, item) in entries.iter().enumerate() {
        let (value, name, writable) = match item {
            ListingEntry::Dirent(dirent) => {
                (&dirent.entry, dirent.entryname.as_ref(), dirent.writable)
            }
            ListingEntry::Expression(expression) => (expression, None, false),
            ListingEntry::Object(_) => return Err("a literal entry of InitialWorkDir".into()),
        };
        let value = match (reference(value, inputs), has_code(value)) {
            (Some(input), _) => Content::Input { input },
            (None, false) => Content::Text(text(value)),
            (None, true) => {
                let port = format!("entry.{index}");
                let port = fill(port, &untrimmed(value)?, Some(Type::Dynamic))?;
                Content::Input { input: port }
            }
        };
        let path = match name {
            None => None,
            Some(name) if !has_code(name) => Some(FileName::Text(text(name))),
            Some(name) => {
                let port = fill(format!("entryname.{index}"), name, Some(Type::String))?;
                Some(FileName::Input { input: port })
            }
        };
        staged.push(entry(value, path, writable));
    }
    Ok(staged)
}

fn entry(value: Content, path: Option<FileName>, writable: bool) -> Staged {
    Staged {
        value,
        path,
        writable,
    }
}

/// The input that an expression is, when it is only `$(inputs.<name>)` of a tool input.
fn reference(expression: &Expression, inputs: &[&str]) -> Option<String> {
    let trimmed = expression.text.trim() == expression.text;
    let [Part::Reference(code)] = expression.parts.as_slice() else {
        return None;
    };
    let name = code.strip_prefix("inputs.")?;
    (trimmed && inputs.contains(&name)).then(|| name.to_owned())
}

fn has_code(expression: &Expression) -> bool {
    (expression.parts.iter()).any(|part| !matches!(part, Part::Text(_)))
}

/// The text of an expression without code.
fn text(expression: &Expression) -> String {
    match expression.parts.as_slice() {
        [Part::Text(text)] => text.clone(),
        parts if !parts.is_empty() => (parts.iter())
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => expression.text.clone(),
    }
}

/// An expression with the white space around it kept as text, as CWL evaluates the entry of
/// a Dirent, while an expr trims it.
fn untrimmed(expression: &Expression) -> Result<Expression, String> {
    let text = &expression.text;
    let leading = &text[..text.len() - text.trim_start().len()];
    let trailing = &text[text.trim_end().len()..];
    if leading.is_empty() && trailing.is_empty() {
        return Ok(expression.clone());
    }
    let mut parts = vec![Part::Text(leading.to_owned())];
    parts.extend(expression.parts.iter().cloned());
    parts.push(Part::Text(trailing.to_owned()));
    parts.retain(|part| !matches!(part, Part::Text(text) if text.is_empty()));
    let code = crate::runtime::joined(&parts);
    Expression::parse(&code, false)
}
