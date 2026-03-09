//! The command line of `iunctus run` and `iunctus config`, with Nextflow's options.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_config::tree;
use nextflow_evaluator::value::Value;

/// The options of a command.
pub struct Arguments {
    /// The script, or the project directory.
    pub target: Option<PathBuf>,
    pub stub: bool,
    pub store: Option<PathBuf>,
    /// `-profile a,b`
    pub profiles: Option<Vec<String>>,
    /// `-c file`
    pub configs: Vec<PathBuf>,
    /// `--name value`, as nested maps of strings.
    pub params: Value,
    /// `-executor local` runs tasks in containers; `-executor mock` only pretends to.
    pub executor: String,
}

/// Reads the options after the command name.
pub fn parse(arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        target: None,
        stub: false,
        store: None,
        profiles: None,
        configs: Vec::new(),
        params: tree::map(),
        executor: "local".into(),
    };
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        index += 1;
        let mut value = || {
            index += 1;
            arguments
                .get(index - 1)
                .cloned()
                .ok_or(format!("the option `{argument}` needs a value"))
        };
        match argument.as_str() {
            "-stub" | "-stub-run" => parsed.stub = true,
            "-store" => {
                let directory = value()?;
                parsed.store = Some(std::path::absolute(&directory).unwrap_or(directory.into()));
            }
            "-profile" => {
                // Groovy's `tokenize(',')`: empty names are dropped, spaces are kept.
                let names = value()?;
                let names = names.split(',').filter(|name| !name.is_empty());
                parsed.profiles = Some(names.map(str::to_owned).collect());
            }
            "-c" | "-config" => {
                let files = value()?;
                parsed.configs.extend(files.split(',').map(PathBuf::from));
            }
            "-executor" => parsed.executor = value()?,
            param if param.starts_with("--") => {
                let (name, text) = match param[2..].split_once('=') {
                    Some((name, text)) => (name.to_owned(), text.to_owned()),
                    None => {
                        let next = arguments.get(index).filter(|next| takes(next));
                        match next {
                            Some(next) => {
                                index += 1;
                                (param[2..].to_owned(), next.clone())
                            }
                            None => (param[2..].to_owned(), "true".into()),
                        }
                    }
                };
                set_param(&parsed.params, &name, &text)?;
            }
            other if parsed.target.is_none() && !other.starts_with('-') => {
                parsed.target = Some(PathBuf::from(other));
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(parsed)
}

/// Whether the argument after `--name` is its value: it does not start with `-`, or it is a
/// number, contains a space or is one character long. An empty argument is not taken.
fn takes(next: &str) -> bool {
    !next.is_empty()
        && (!next.starts_with('-')
            || next.parse::<f64>().is_ok()
            || next.contains(' ')
            || next.chars().count() == 1)
}

/// `--a.b-c value` sets `params.a.bC`: dots nest, `\.` is a literal dot, and the last part
/// changes from kebab case to camel case.
fn set_param(params: &Value, name: &str, text: &str) -> Result<(), String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&'.') => {
                characters.next();
                parts.last_mut().expect("a part").push('.');
            }
            '.' => parts.push(String::new()),
            other => parts.last_mut().expect("a part").push(other),
        }
    }
    let last = parts.pop().expect("a part");
    parts.push(camel_case(&last));
    let path: Vec<Rc<str>> = parts.iter().map(|part| part.as_str().into()).collect();
    tree::assign(params, &path, Value::string(text))
}

/// `kebab-case` becomes `kebabCase`.
fn camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for character in name.chars() {
        match character {
            '-' => upper = true,
            other if upper => {
                result.extend(other.to_uppercase());
                upper = false;
            }
            other => result.push(other),
        }
    }
    result
}
