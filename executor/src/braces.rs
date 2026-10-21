//! Brace expansion of output patterns. Bash expands braces before it expands variables, so the
//! wrapper cannot leave `*.{html,zip}` in a variable to Bash; the patterns are expanded here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The patterns that the shell's brace expansion makes of a pattern: `*.{html,zip}` gives
/// `*.html` and `*.zip`. Groups nest, and a group without a comma stays as it is, as in Bash.
pub fn expand_braces(pattern: &str) -> Vec<String> {
    let mut start = 0;
    while let Some(offset) = pattern[start..].find('{') {
        let open = start + offset;
        if let Some((close, choices)) = group(pattern, open) {
            let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
            return choices
                .into_iter()
                .flat_map(|choice| expand_braces(&format!("{head}{choice}{tail}")))
                .collect();
        }
        start = open + 1;
    }
    vec![pattern.to_owned()]
}

/// Where the group that opens at `open` closes, and its choices; `None` when it does not close
/// or has no comma of its own.
fn group(pattern: &str, open: usize) -> Option<(usize, Vec<&str>)> {
    let mut depth = 0;
    let mut choices = Vec::new();
    let mut from = open + 1;
    for (offset, character) in pattern[open..].char_indices() {
        let index = open + offset;
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    choices.push(&pattern[from..index]);
                    return (choices.len() > 1).then_some((index, choices));
                }
            }
            ',' if depth == 1 => {
                choices.push(&pattern[from..index]);
                from = index + 1;
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_bash_braces() {
        let expand = |pattern: &str| expand_braces(pattern).join(" ");
        assert_eq!(expand("*.{html,zip}"), "*.html *.zip");
        assert_eq!(expand("{a,b}{1,2}"), "a1 a2 b1 b2");
        assert_eq!(expand("x{a,b{c,d}}y"), "xay xbcy xbdy");
        assert_eq!(expand("{}{x}{a,}"), "{}{x}a {}{x}");
        assert_eq!(expand("open{a,b"), "open{a,b");
    }
}
