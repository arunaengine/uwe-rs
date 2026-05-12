//! Glob patterns of files, as `file()`, `files()` and `channel.fromPath` expand them. Matches
//! are sorted by path, which the design chooses over Nextflow's file tree order.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;

use super::{Error, Host, thrown};
use crate::exception::Exception;
use crate::regex::Pattern;
use crate::value::PathValue;

/// What a glob may match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Any,
}

/// The options of a glob, as `channel.fromPath` takes them.
#[derive(Clone, Debug)]
pub struct GlobOptions {
    pub file_type: FileType,
    /// Whether hidden files match; by default only when the pattern's name starts with `.`.
    pub hidden: Option<bool>,
    /// How many directories deep matches may be; by default unlimited with `**`, else the
    /// number of directories in the pattern.
    pub max_depth: Option<usize>,
}

impl Default for GlobOptions {
    fn default() -> Self {
        Self {
            file_type: FileType::File,
            hidden: None,
            max_depth: None,
        }
    }
}

/// Whether text has glob characters.
pub fn is_glob(text: &str) -> bool {
    text.contains(['*', '?', '[', '{'])
}

/// The files that match a glob pattern; a relative pattern starts at the launch directory.
pub fn glob(
    host: &mut dyn Host,
    pattern: &str,
    options: &GlobOptions,
) -> Result<Vec<PathValue>, Error> {
    // A remote pattern, like one of an S3 bucket, lists the directories of the host.
    let path = PathValue::parse(pattern, &host.launch_dir());
    let text = path.path.clone();
    let first = text.find(['*', '?', '[', '{']).unwrap_or(text.len());
    let split = text[..first].rfind('/').map_or(0, |slash| slash + 1);
    let (base, glob) = (&text[..split], &text[split..]);
    let base = if base.is_empty() { "/" } else { base };
    let max_depth = options.max_depth.unwrap_or(if glob.contains("**") {
        usize::MAX
    } else {
        glob.matches('/').count()
    });
    let name = glob.rsplit('/').next().unwrap_or(glob);
    let hidden = options.hidden.unwrap_or(name.starts_with('.'));
    let matcher =
        Pattern::new(&glob_regex(glob)).map_err(|exception| thrown(exception, Span::default()))?;
    let mut found = Vec::new();
    let root = PathValue {
        path: base.to_owned(),
        staged: None,
        relative: None,
        prefix: path.prefix.clone(),
    };
    let mut pending = vec![(root, String::new(), 0usize)];
    while let Some((directory, relative, depth)) = pending.pop() {
        let Ok(entries) = host.list(&directory) else {
            continue;
        };
        for (entry, is_directory) in entries {
            if !hidden && entry.starts_with('.') {
                continue;
            }
            let path = directory.resolve(&entry);
            let relative = if relative.is_empty() {
                entry.clone()
            } else {
                format!("{relative}/{entry}")
            };
            let wanted = match options.file_type {
                FileType::File => !is_directory,
                FileType::Directory => is_directory,
                FileType::Any => true,
            };
            if wanted && matcher.matches(&relative) {
                found.push(path.clone());
            }
            if is_directory && depth < max_depth {
                pending.push((path, relative, depth + 1));
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Whether a relative file name matches a glob pattern, as output patterns match task files.
/// A pattern that ends with `/`, like `db/`, matches the directory.
pub fn matches(glob: &str, name: &str) -> bool {
    let glob = glob.strip_suffix('/').unwrap_or(glob);
    Pattern::new(&glob_regex(glob)).is_ok_and(|pattern| pattern.matches(name))
}

/// Java's glob syntax as a regular expression: `**` crosses directories, `*` and `?` do not,
/// `{a,b}` is a choice and `[...]` a character class.
fn glob_regex(glob: &str) -> String {
    let mut regex = String::new();
    let mut characters = glob.chars().peekable();
    let mut in_group = false;
    while let Some(character) = characters.next() {
        match character {
            '*' if characters.next_if_eq(&'*').is_some() => regex.push_str(".*"),
            '*' => regex.push_str("[^/]*"),
            '?' => regex.push_str("[^/]"),
            '{' => {
                in_group = true;
                regex.push_str("(?:");
            }
            '}' if in_group => {
                in_group = false;
                regex.push(')');
            }
            ',' if in_group => regex.push('|'),
            '[' => {
                regex.push('[');
                if characters.next_if_eq(&'!').is_some() {
                    regex.push('^');
                }
                for inner in characters.by_ref() {
                    regex.push(inner);
                    if inner == ']' {
                        break;
                    }
                }
            }
            '\\' => {
                if let Some(next) = characters.next() {
                    regex.push_str(&escape(next));
                }
            }
            other => regex.push_str(&escape(other)),
        }
    }
    regex
}

fn escape(character: char) -> String {
    if character.is_alphanumeric() || character == '/' || character == '_' {
        character.to_string()
    } else {
        format!("\\{character}")
    }
}

/// Nextflow's error when `checkIfExists` finds no file for a pattern.
pub fn no_match(pattern: &str) -> Error {
    let message = format!("No files match pattern `{pattern}`");
    thrown(
        Exception::new("NoSuchFileException", message),
        Span::default(),
    )
}
