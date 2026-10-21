//! Maps supported container flags to task environments and resource requests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::{Error, TaskDirective};
use nextflow_evaluator::value::Value;
use std::collections::BTreeMap;
use workflow_graph::Resources;

pub(crate) const FLAGS_ENV: &str = "IUNCTUS_CONTAINER_FLAGS";
pub(crate) const WORK_ENV: &str = "IUNCTUS_WORK_VARIABLES";
const WORK_MARKER: &str = "\0PWD\0";

pub(crate) fn apply(
    directives: &[TaskDirective],
    resources: &mut Resources,
    environment: &mut BTreeMap<String, String>,
) -> Result<(), Error> {
    let mut flags = std::collections::BTreeMap::new();
    let mut work = Vec::new();
    for directive in directives
        .iter()
        .filter(|directive| &*directive.name == "containerOptions")
    {
        let text = directive
            .arguments
            .iter()
            .map(|value| match value {
                Value::Map(map) => map
                    .borrow()
                    .entries
                    .iter()
                    .map(|(name, value)| {
                        format!(
                            "--{}='{}'",
                            name.0,
                            value.to_string().replace('\'', "'\\''")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::List(list) => list
                    .borrow()
                    .items
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::Null => String::new(),
                value => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let words = words(&text)?;
        let mut words = words.iter();
        while let Some(word) = words.next() {
            let (flag, inline) =
                if word.starts_with("-e") && !word.starts_with("--") && word.len() > 2 {
                    ("-e", Some(&word[2..]))
                } else {
                    word.split_once('=')
                        .map_or((word.as_str(), None), |(flag, value)| (flag, Some(value)))
                };
            if !matches!(
                flag,
                "-e" | "--env" | "--cpus" | "--gpus" | "--shm-size" | "--memory-swap"
            ) {
                return Err(refused(flag));
            }
            let value = inline
                .or_else(|| words.next().map(String::as_str))
                .ok_or_else(|| refused(flag))?;
            match flag {
                "-e" | "--env" => {
                    let (name, value) = value
                        .split_once('=')
                        .ok_or_else(|| refused("environment inheritance without a value"))?;
                    if name.is_empty()
                        || name.bytes().enumerate().any(|(index, byte)| {
                            !(byte == b'_'
                                || byte.is_ascii_alphabetic()
                                || index > 0 && byte.is_ascii_digit())
                        })
                    {
                        return Err(refused("invalid environment name"));
                    }
                    if value == WORK_MARKER {
                        work.push(name.to_owned());
                    } else if value.contains(WORK_MARKER) {
                        return Err(refused("work directory expressions other than PWD"));
                    } else {
                        environment.insert(name.to_owned(), value.to_owned());
                    }
                }
                "--cpus" => {
                    let cpus: f64 = value.parse().map_err(|_| refused(flag))?;
                    if !cpus.is_finite() || cpus <= 0.0 || cpus > i32::MAX as f64 {
                        return Err(refused(flag));
                    }
                    resources.cpus = Some(cpus);
                    flags.insert("cpus", (cpus * 1_000_000_000.0) as i64);
                }
                "--gpus" => {
                    let count = if value == "all" {
                        -1
                    } else {
                        value
                            .parse::<i32>()
                            .map_err(|_| refused("GPU selections other than a count or all"))?
                            as i64
                    };
                    if count < -1 {
                        return Err(refused(flag));
                    }
                    resources.gpus = (count >= 0).then_some(count as u32);
                    flags.insert("gpus", count);
                }
                "--shm-size" | "--memory-swap" => {
                    let bytes = if flag == "--memory-swap" && value == "-1" {
                        -1
                    } else {
                        bytes(value).map_err(|_| refused(flag))?
                    };
                    flags.insert(if flag == "--shm-size" { "shm" } else { "swap" }, bytes);
                }
                _ => unreachable!(),
            }
        }
    }
    if !flags.is_empty() {
        environment.insert(FLAGS_ENV.into(), serde_json::to_string(&flags).unwrap());
    }
    if !work.is_empty() {
        environment.insert(WORK_ENV.into(), serde_json::to_string(&work).unwrap());
    }
    Ok(())
}

fn words(text: &str) -> Result<Vec<String>, Error> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut present = false;
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\\' && quote != Some('\'') {
            let next = characters
                .next()
                .ok_or_else(|| refused("unclosed quoting"))?;
            if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                word.push('\\');
            }
            if next != '\n' {
                word.push(next);
            }
            present = true;
        } else if character == '$' && quote != Some('\'') {
            let mut expected = "PWD".chars();
            let first = characters.next();
            if first == Some('{') {
                for character in expected {
                    if characters.next() != Some(character) {
                        return Err(refused("shell expansion"));
                    }
                }
                if characters.next() != Some('}') {
                    return Err(refused("shell expansion"));
                }
            } else {
                if first != expected.next() {
                    return Err(refused("shell expansion"));
                }
                for character in expected {
                    if characters.next() != Some(character) {
                        return Err(refused("shell expansion"));
                    }
                }
            }
            word.push_str(WORK_MARKER);
            present = true;
        } else if character == '`' && quote != Some('\'') {
            return Err(refused("shell expansion"));
        } else if quote.is_none() && matches!(character, ';' | '|' | '&' | '<' | '>' | '(' | ')') {
            return Err(refused("shell operators"));
        } else if Some(character) == quote {
            quote = None;
        } else if quote.is_some() {
            word.push(character);
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
            present = true;
        } else if character.is_whitespace() {
            if present {
                words.push(std::mem::take(&mut word));
                present = false;
            }
        } else {
            word.push(character);
            present = true;
        }
    }
    if quote.is_some() {
        return Err(refused("unclosed quoting"));
    }
    if present {
        words.push(word);
    }
    Ok(words)
}

fn refused(flag: &str) -> Error {
    Error::Unsupported {
        message: format!("process directive `containerOptions`: {flag}"),
        span: Default::default(),
    }
}

fn bytes(text: &str) -> Result<i64, ()> {
    let at = text
        .rfind(|character: char| character.is_ascii_digit() || matches!(character, '.' | ' '))
        .ok_or(())?;
    let (number, suffix) = if text.as_bytes()[at] == b' ' {
        (&text[..at], &text[at + 1..])
    } else {
        (&text[..at + 1], &text[at + 1..])
    };
    let number: f64 = number.parse().map_err(|_| ())?;
    let power = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kb" | "kib" => 1,
        "m" | "mb" | "mib" => 2,
        "g" | "gb" | "gib" => 3,
        "t" | "tb" | "tib" => 4,
        "p" | "pb" | "pib" => 5,
        _ => return Err(()),
    };
    let bytes = number * 1024f64.powi(power);
    if !bytes.is_finite() || bytes < 0.0 || bytes >= i64::MAX as f64 {
        return Err(());
    }
    Ok(bytes as i64)
}
