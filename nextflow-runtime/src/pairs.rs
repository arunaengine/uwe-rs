//! `channel.fromFilePairs`: the files of glob patterns grouped by the start of their names,
//! as Nextflow's `Channel.fromFilePairs` and `readPrefix` group them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::interpreter::glob::is_glob;
use nextflow_evaluator::value::{PathValue, Value};

/// The tuples of the files each pattern found: `[key, [files]]`, or `[key, file, ...]` when
/// `flat`. A group is emitted when it holds `size` files, sorted; a `size` below one emits
/// every group at the end, and groups that stay smaller than `size` are dropped.
pub(crate) fn file_pairs(
    found: Vec<(String, Vec<Value>)>,
    size: Option<i64>,
    flat: bool,
) -> Result<Vec<Value>, Error> {
    let any_pattern = found.iter().any(|(pattern, _)| is_glob(pattern));
    let size = size
        .filter(|size| *size != 0)
        .unwrap_or(if any_pattern { 2 } else { 1 });
    let mut groups: Vec<(Option<String>, Vec<PathValue>)> = Vec::new();
    let mut tuples = Vec::new();
    for (pattern, paths) in found {
        for path in paths {
            let Value::Path(path) = path else { continue };
            let key = read_prefix(&path, &pattern)?;
            let index = match groups.iter().position(|(other, _)| *other == key) {
                Some(index) => index,
                None => {
                    groups.push((key, Vec::new()));
                    groups.len() - 1
                }
            };
            groups[index].1.push((*path).clone());
            if size > 0 && groups[index].1.len() as i64 == size {
                let (key, files) = groups.remove(index);
                tuples.push(tuple(key, files, flat));
            }
        }
    }
    if size <= 0 {
        for (key, files) in groups {
            tuples.push(tuple(key, files, flat));
        }
    }
    Ok(tuples)
}

fn tuple(key: Option<String>, mut files: Vec<PathValue>, flat: bool) -> Value {
    let key = key.map_or(Value::Null, Value::string);
    files.sort_by_key(PathValue::uri);
    let files = files.into_iter().map(|path| Value::Path(Rc::new(path)));
    match flat {
        true => Value::list(std::iter::once(key).chain(files).collect()),
        false => Value::list(vec![key, Value::list(files.collect())]),
    }
}

/// The start of the file name that the pattern matches up to its last wildcard before any
/// `{}` or `[]`, without trailing `-`, `_` or `.`: `file_alpha_1.fa` and `file*_1.fa` give
/// `file_alpha`. `None` when the name does not match.
fn read_prefix(path: &PathValue, pattern: &str) -> Result<Option<String>, Error> {
    let name = path.name();
    let pattern = pattern.rsplit('/').next().unwrap_or(pattern);
    let wildcard = pattern.find(['*', '?']);
    let bracket = pattern.find(['{', '[']);
    if wildcard.is_none() && bracket.is_none() {
        if name == pattern {
            return Ok(Some(path.simple_name().to_owned()));
        }
        let message =
            format!("Not a valid file pair globbing pattern: pattern={pattern} file={name}");
        return Err(Error::Thrown {
            exception: Rc::new(Exception::illegal_argument(message)),
            span: Default::default(),
        });
    }
    let groups = pattern
        .chars()
        .take_while(|c| !matches!(c, '{' | '['))
        .filter(|c| matches!(c, '*' | '?'))
        .count();
    let regex = pattern
        .replace('.', "\\.")
        .replace('*', "(.*)")
        .replace('?', "(.?)")
        .replace('{', "(?:")
        .replace('}', ")")
        .replace(',', "|");
    let Ok(regex) = regex::Regex::new(&format!("^(?:{regex})$")) else {
        return Ok(None);
    };
    let Some(captures) = regex.captures(name) else {
        return Ok(None);
    };
    let count = groups.min(captures.len() - 1);
    let end = match count {
        0 => bracket.unwrap_or(name.len()),
        _ => captures.get(count).map_or(0, |group| group.end()),
    };
    let prefix = name.get(..end).unwrap_or(name);
    Ok(Some(prefix.trim_end_matches(['-', '_', '.']).to_owned()))
}
