//! Word expansions of the shell for the simulator: variables, brace
//! expansion, the escapes of `echo -e` and `printf`, and paths relative to the working
//! directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use super::lexer::{Piece, dollar};

/// A path as the working directory sees it: without `./` and a trailing `/`.
pub fn normal(path: &str) -> String {
    let mut path = path;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.trim_end_matches('/').to_owned()
}

/// Brace expansion: `a.{b,c}` gives `a.b` and `a.c`; `{1..3}` gives `1`, `2` and `3`.
pub fn braces(word: &str) -> Vec<String> {
    let Some(open) = word.find('{') else {
        return vec![word.to_owned()];
    };
    let mut depth = 0;
    let mut close = None;
    for (index, c) in word[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![word.to_owned()];
    };
    let (prefix, inner, suffix) = (&word[..open], &word[open + 1..close], &word[close + 1..]);
    let choices: Vec<String> = if let Some((from, to)) = inner.split_once("..")
        && let (Ok(from), Ok(to)) = (from.parse::<i64>(), to.parse::<i64>())
    {
        let range: Vec<i64> = if from <= to {
            (from..=to).collect()
        } else {
            (to..=from).rev().collect()
        };
        range.iter().map(i64::to_string).collect()
    } else if inner.contains(',') {
        split_top(inner)
    } else {
        return braces(suffix)
            .into_iter()
            .map(|rest| format!("{prefix}{{{inner}}}{rest}"))
            .collect();
    };
    let mut words = Vec::new();
    for choice in choices {
        for rest in braces(&format!("{choice}{suffix}")) {
            words.push(format!("{prefix}{rest}"));
        }
    }
    words
}

/// The parts of a brace list, split at commas outside inner braces.
fn split_top(inner: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut depth = 0;
    for c in inner.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().expect("a part").push(c);
    }
    parts
}

/// `echo -e` escapes.
pub fn escapes(text: &str) -> String {
    text.replace("\\n", "\n").replace("\\t", "\t")
}

/// `printf format values...`: `%s` and `%d` take the next value, with the escapes of
/// `echo -e`; the format repeats while values are left.
pub fn printf(format: &str, values: &[String]) -> String {
    let mut out = String::new();
    let mut values = values.iter();
    loop {
        let mut used = false;
        let mut characters = format.chars().peekable();
        while let Some(c) = characters.next() {
            match c {
                '%' if characters.peek() == Some(&'%') => {
                    characters.next();
                    out.push('%');
                }
                '%' => {
                    // Flags and width are left out, up to the conversion letter.
                    while characters.next().is_some_and(|c| !c.is_ascii_alphabetic()) {}
                    used = true;
                    out.push_str(values.next().map_or("", String::as_str));
                }
                '\\' => match characters.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                },
                other => out.push(other),
            }
        }
        if !used || values.len() == 0 {
            return out;
        }
    }
}

/// The text of a word, or `None` when a part of it is not known.
pub fn expand(pieces: &[Piece], variables: &HashMap<String, String>) -> Option<String> {
    let mut text = String::new();
    for piece in pieces {
        match piece {
            Piece::Text { text: part, .. } => text.push_str(part),
            Piece::Variable { name, .. } => text.push_str(variables.get(name)?),
            Piece::Command(_) | Piece::Unknown => return None,
        }
    }
    Some(text)
}

/// The words that a word expands to, with brace expansion of its unquoted text.
pub fn expand_all(pieces: &[Piece], variables: &HashMap<String, String>) -> Option<Vec<String>> {
    let text = expand(pieces, variables)?;
    let unquoted_braces = pieces.iter().any(|piece| {
        matches!(piece, Piece::Text { text, quoted: false }
            if text.contains('{') && (text.contains(',') || text.contains("..")))
    });
    let unquoted_glob = pieces.iter().any(|piece| {
        matches!(piece, Piece::Text { text, quoted: false } if text.contains(['*', '?', '[']))
    });
    if unquoted_glob {
        // A glob names files that the simulation cannot list.
        return None;
    }
    Some(if unquoted_braces {
        braces(&text)
    } else {
        vec![text]
    })
}

/// The text of a here document, with variables expanded; commands in it give nothing.
pub fn expand_text(body: &str, variables: &HashMap<String, String>) -> String {
    let pieces = {
        let mut pieces = Vec::new();
        let characters: Vec<char> = body.chars().collect();
        let mut index = 0;
        let mut text = String::new();
        while index < characters.len() {
            match characters[index] {
                '\\' if matches!(characters.get(index + 1), Some('$' | '`' | '\\')) => {
                    text.push(characters[index + 1]);
                    index += 2;
                }
                '$' | '`' => {
                    pieces.push(Piece::Text {
                        text: std::mem::take(&mut text),
                        quoted: true,
                    });
                    index = dollar(&characters, index, true, &mut pieces);
                }
                other => {
                    text.push(other);
                    index += 1;
                }
            }
        }
        pieces.push(Piece::Text { text, quoted: true });
        pieces
    };
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Text { text, .. } => text.clone(),
            Piece::Variable { name, .. } => variables.get(name).cloned().unwrap_or_default(),
            Piece::Command(_) | Piece::Unknown => String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braces_expand() {
        assert_eq!(
            braces("s.{a,b}.{x,y}"),
            ["s.a.x", "s.a.y", "s.b.x", "s.b.y"]
        );
        assert_eq!(braces("part{1..3}"), ["part1", "part2", "part3"]);
        assert_eq!(braces("{a}"), ["{a}"]);
    }

    #[test]
    fn printf_and_paths() {
        assert_eq!(
            printf("%s\\t%s\\n", &["a".into(), "b".into(), "c".into()]),
            "a\tb\nc\t\n"
        );
        assert_eq!(normal("./out/"), "out");
    }
}
