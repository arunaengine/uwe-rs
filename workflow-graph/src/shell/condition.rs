//! The conditions of `if` statements that the simulation can decide: `[ ... ]`, `[[ ... ]]`
//! and `test ...` with known words, like `[ "false" == true ]` in a stub script.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// What a path of the working directory is, as far as the simulation knows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Entry {
    File,
    Directory,
    /// An input of the task, which may be a file or a directory.
    Input,
    Absent,
}

/// Whether a test command is true, or `None` when the simulation cannot tell. `words` are the
/// expanded words of the command, like `[`, `a`, `==`, `b`, `]`; `entry` tells what a relative
/// path is.
pub fn decide(words: &[String], entry: impl Fn(&str) -> Entry) -> Option<bool> {
    let (first, rest) = words.split_first()?;
    let (arguments, pattern) = match first.as_str() {
        "[" => (rest.strip_suffix(&["]".to_owned()])?, false),
        "[[" => (rest.strip_suffix(&["]]".to_owned()])?, true),
        "test" => (rest, false),
        _ => return None,
    };
    let (negated, arguments) = match arguments.split_first() {
        Some((not, rest)) if not == "!" => (true, rest),
        _ => (false, arguments),
    };
    let result = match arguments {
        [word] => !word.is_empty(),
        [operator, word] => match operator.as_str() {
            "-z" => word.is_empty(),
            "-n" => !word.is_empty(),
            "-e" | "-f" | "-d" if !word.starts_with('/') => {
                match (operator.as_str(), entry(word)) {
                    (_, Entry::Absent) => false,
                    (_, Entry::Input) if operator != "-e" => return None,
                    ("-f", found) => found != Entry::Directory,
                    ("-d", found) => found == Entry::Directory,
                    _ => true,
                }
            }
            _ => return None,
        },
        [left, operator, right] => match operator.as_str() {
            // In `[[ ]]` the right side of `==` is a pattern.
            "==" | "=" | "!=" if pattern && right.contains(['*', '?', '[']) => return None,
            "==" | "=" => left == right,
            "!=" => left != right,
            "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                let left: i64 = left.trim().parse().ok()?;
                let right: i64 = right.trim().parse().ok()?;
                match operator.as_str() {
                    "-eq" => left == right,
                    "-ne" => left != right,
                    "-lt" => left < right,
                    "-le" => left <= right,
                    "-gt" => left > right,
                    _ => left >= right,
                }
            }
            _ => return None,
        },
        _ => return None,
    };
    Some(result != negated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(text: &str) -> Option<bool> {
        let words: Vec<String> = text.split(' ').map(str::to_owned).collect();
        decide(&words, |path| match path {
            "made.txt" => Entry::File,
            "db" => Entry::Directory,
            "in.bam" => Entry::Input,
            _ => Entry::Absent,
        })
    }

    #[test]
    fn evaluates_known_conditions() {
        assert_eq!(test("[ false == true ]"), Some(false));
        assert_eq!(test("[ a = a ]"), Some(true));
        assert_eq!(test("[[ a != b ]]"), Some(true));
        assert_eq!(test("test 2 -gt 1"), Some(true));
        assert_eq!(test("[ ! -z x ]"), Some(true));
        assert_eq!(test("[ -f made.txt ]"), Some(true));
        assert_eq!(test("[ -d made.txt ]"), Some(false));
        assert_eq!(test("[ -e in.bam ]"), Some(true));
        assert_eq!(test("[ -f other.txt ]"), Some(false));
    }

    #[test]
    fn unknown_conditions_open() {
        assert_eq!(test("[ -f in.bam ]"), None);
        assert_eq!(test("[ -s made.txt ]"), None);
        assert_eq!(test("[ -f /etc/hosts ]"), None);
        assert_eq!(test("[[ abc == a* ]]"), None);
        assert_eq!(test("[ x -eq 1 ]"), None);
        assert_eq!(test("grep -q x in.bam"), None);
    }
}
