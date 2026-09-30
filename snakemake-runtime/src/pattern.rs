//! File patterns with wildcards, like `results/{sample}.{unit,\d+}.bam`: parsing, matching a
//! file to them, and filling them in, as Snakemake's `io` module does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use fancy_regex::Regex;
use indexmap::IndexMap;

/// Wildcard values by name, in the order the pattern names them.
pub type Wildcards = IndexMap<String, String>;

/// One `{name}` or `{name,constraint}` in a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wildcard {
    pub name: String,
    pub constraint: Option<String>,
    /// Byte range of the braces and their content.
    pub start: usize,
    pub end: usize,
}

/// Reads a wildcard that starts with the `{` at `start`, following Snakemake's
/// `WILDCARD_REGEX`: optional space, a word, then optionally a comma and a constraint in
/// which braces nest one level, as in `{w,a{3,5}}`.
fn wildcard_at(pattern: &str, start: usize) -> Option<Wildcard> {
    let bytes = pattern.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let name_start = index;
    while index < bytes.len()
        && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_' || bytes[index] >= 0x80)
    {
        index += 1;
    }
    if index == name_start {
        return None;
    }
    let name = pattern[name_start..index].to_owned();
    let mut probe = index;
    while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
        probe += 1;
    }
    let mut constraint = None;
    if probe < bytes.len() && bytes[probe] == b',' {
        let mut position = probe + 1;
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        let constraint_start = position;
        loop {
            match bytes.get(position) {
                Some(b'{') => {
                    // Only a repetition count like {3} or {3,5} nests.
                    let close = pattern[position..].find('}')?;
                    let inner = &pattern[position + 1..position + close];
                    let valid = !inner.is_empty()
                        && inner.split(',').count() <= 2
                        && inner.split(',').all(|part| {
                            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                        });
                    if !valid {
                        break;
                    }
                    position += close + 1;
                }
                Some(b'}') | None => break,
                Some(_) => position += 1,
            }
        }
        if position > constraint_start {
            // Trailing space before the closing brace is not part of the constraint.
            let text = pattern[constraint_start..position].to_owned();
            constraint = Some(text);
        }
        index = position;
    } else {
        index = probe;
    }
    if bytes.get(index) != Some(&b'}') {
        return None;
    }
    Some(Wildcard {
        name,
        constraint,
        start,
        end: index + 1,
    })
}

/// The wildcards of a pattern, in order.
pub fn wildcards(pattern: &str) -> Vec<Wildcard> {
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(offset) = pattern[index..].find('{') {
        let start = index + offset;
        match wildcard_at(pattern, start) {
            Some(wildcard) => {
                index = wildcard.end;
                found.push(wildcard);
            }
            None => index = start + 1,
        }
    }
    found
}

/// The distinct wildcard names of a pattern, in order.
pub fn wildcard_names(pattern: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for wildcard in wildcards(pattern) {
        if !names.contains(&wildcard.name) {
            names.push(wildcard.name);
        }
    }
    names
}

/// The pattern without the constraints of its wildcards, as `strip_wildcard_constraints`
/// gives it: `{sample,[A-Z]+}` becomes `{sample}`.
pub fn strip_constraints(pattern: &str) -> String {
    let mut output = String::with_capacity(pattern.len());
    let mut last = 0;
    for wildcard in wildcards(pattern) {
        output.push_str(&pattern[last..wildcard.start]);
        output.push('{');
        output.push_str(&wildcard.name);
        output.push('}');
        last = wildcard.end;
    }
    output.push_str(&pattern[last..]);
    output
}

/// Fills in the wildcards of a pattern; the name of a missing wildcard is the error.
pub fn apply(pattern: &str, values: &Wildcards) -> Result<String, String> {
    let mut output = String::with_capacity(pattern.len());
    let mut last = 0;
    for wildcard in wildcards(pattern) {
        output.push_str(&pattern[last..wildcard.start]);
        match values.get(&wildcard.name) {
            Some(value) => output.push_str(value),
            None => return Err(wildcard.name),
        }
        last = wildcard.end;
    }
    output.push_str(&pattern[last..]);
    Ok(output)
}

/// The pattern with constraints of the rule or the workflow added to wildcards that have
/// none, as `update_wildcard_constraints` does.
pub fn update_constraints(
    pattern: &str,
    rule: &IndexMap<String, String>,
    global: &IndexMap<String, String>,
) -> String {
    let mut output = String::with_capacity(pattern.len());
    let mut last = 0;
    let mut seen: Vec<String> = Vec::new();
    for wildcard in wildcards(pattern) {
        output.push_str(&pattern[last..wildcard.start]);
        let text = &pattern[wildcard.start..wildcard.end];
        let first = !seen.contains(&wildcard.name);
        seen.push(wildcard.name.clone());
        let constraint = rule
            .get(&wildcard.name)
            .or_else(|| global.get(&wildcard.name));
        match (first, &wildcard.constraint, constraint) {
            (true, None, Some(constraint)) => {
                output.push_str(&format!("{{{},{constraint}}}", wildcard.name));
            }
            _ => output.push_str(text),
        }
        last = wildcard.end;
    }
    output.push_str(&pattern[last..]);
    output
}

/// The regular expression of a pattern, as `regex_from_filepattern` builds it.
pub fn regex_text(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut last = 0;
    let mut seen: Vec<String> = Vec::new();
    for wildcard in wildcards(pattern) {
        regex.push_str(&fancy_regex::escape(&pattern[last..wildcard.start]));
        if seen.contains(&wildcard.name) {
            regex.push_str(&format!("(?P={})", wildcard.name));
        } else {
            let constraint = wildcard
                .constraint
                .as_deref()
                .map(python_evaluator::modules::translate_regex)
                .unwrap_or_else(|| ".+".into());
            regex.push_str(&format!("(?P<{}>{constraint})", wildcard.name));
            seen.push(wildcard.name.clone());
        }
        last = wildcard.end;
    }
    regex.push_str(&fancy_regex::escape(&pattern[last..]));
    regex.push('$');
    regex
}

/// A compiled pattern for matching files.
pub struct Matcher {
    pub pattern: String,
    regex: Option<Regex>,
    names: Vec<String>,
    /// The text before the first wildcard and after the last, to rule out most files fast.
    prefix: String,
    suffix: String,
}

thread_local! {
    static MATCHERS: std::cell::RefCell<HashMap<String, Rc<Matcher>>> = std::cell::RefCell::new(HashMap::new());
}

impl Matcher {
    /// The matcher of a pattern; matchers are cached.
    pub fn of(pattern: &str) -> Rc<Matcher> {
        if let Some(found) = MATCHERS.with(|cache| cache.borrow().get(pattern).cloned()) {
            return found;
        }
        let found = wildcards(pattern);
        let (prefix, suffix) = match (found.first(), found.last()) {
            (Some(first), Some(last)) => (
                pattern[..first.start].to_owned(),
                pattern[last.end..].to_owned(),
            ),
            _ => (pattern.to_owned(), pattern.to_owned()),
        };
        let matcher = Rc::new(Matcher {
            pattern: pattern.to_owned(),
            regex: Regex::new(&regex_text(pattern)).ok(),
            names: wildcard_names(pattern),
            prefix,
            suffix,
        });
        MATCHERS.with(|cache| {
            cache
                .borrow_mut()
                .insert(pattern.to_owned(), matcher.clone())
        });
        matcher
    }

    /// Whether the pattern could be compiled; a constraint may be an invalid regex.
    pub fn is_valid(&self) -> bool {
        self.regex.is_some()
    }

    /// The wildcard values if the file matches the whole pattern.
    pub fn matches(&self, file: &str) -> Option<Wildcards> {
        if self.names.is_empty() {
            return (file == self.pattern).then(Wildcards::new);
        }
        if !file.starts_with(&self.prefix) || !file.ends_with(&self.suffix) {
            return None;
        }
        let captures = self.regex.as_ref()?.captures(file).ok()??;
        let mut values = Wildcards::new();
        for name in &self.names {
            let value = captures.name(name)?.as_str().to_owned();
            values.insert(name.clone(), value);
        }
        Some(values)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Whether a wildcard value satisfies the constraints written in a pattern.
pub fn satisfies_constraints(pattern: &str, values: &Wildcards) -> bool {
    for wildcard in wildcards(pattern) {
        let (Some(constraint), Some(value)) = (&wildcard.constraint, values.get(&wildcard.name))
        else {
            continue;
        };
        let regex = format!(
            "^(?:{})",
            python_evaluator::modules::translate_regex(constraint)
        );
        if !Regex::new(&regex)
            .ok()
            .and_then(|regex| regex.is_match(value).ok())
            .unwrap_or(false)
        {
            return false;
        }
    }
    true
}

/// Whether a value ends with one part repeated many times, like `a.gz.gz.gz...`: a sign
/// that a rule feeds on its own output, as Snakemake's `PeriodicityDetector` finds.
pub fn is_periodic(value: &str, repeats: usize) -> bool {
    let bytes = value.as_bytes();
    let longest = bytes.len() / repeats;
    (1..=longest).any(|period| {
        let tail = &bytes[bytes.len() - period..];
        (1..repeats).all(|copy| {
            let end = bytes.len() - copy * period;
            &bytes[end - period..end] == tail
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> Wildcards {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn finds_pattern_wildcards() {
        let found = wildcards("a/{sample}/{unit, \\d+}.{ext,a{2,3}}{{x}}");
        let names: Vec<&str> = found.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["sample", "unit", "ext", "x"]);
        assert_eq!(found[1].constraint.as_deref(), Some("\\d+"));
        assert_eq!(found[2].constraint.as_deref(), Some("a{2,3}"));
        assert!(wildcards("{} {-x} {a b}").is_empty());
        // Python's `\w` takes digits, so `{1}` is a wildcard.
        assert_eq!(wildcards("{1}")[0].name, "1");
    }

    #[test]
    fn files_match_patterns() {
        let matcher = Matcher::of("results/{sample}/{sample}.{unit,\\d+}.bam");
        assert_eq!(
            matcher.matches("results/A/A.12.bam"),
            Some(values(&[("sample", "A"), ("unit", "12")]))
        );
        assert_eq!(matcher.matches("results/A/B.12.bam"), None);
        assert_eq!(matcher.matches("results/A/A.x.bam"), None);
        assert_eq!(
            Matcher::of("plain.txt").matches("plain.txt"),
            Some(Wildcards::new())
        );
    }

    #[test]
    fn fills_pattern_values() {
        let filled = apply("{a}/{b,\\w+}.txt", &values(&[("a", "x"), ("b", "y")]));
        assert_eq!(filled, Ok("x/y.txt".to_owned()));
        assert_eq!(apply("{a}.txt", &Wildcards::new()), Err("a".to_owned()));
    }

    #[test]
    fn avoids_duplicate_constraints() {
        let rule: IndexMap<String, String> = [("a".to_owned(), "[0-9]+".to_owned())]
            .into_iter()
            .collect();
        let global: IndexMap<String, String> =
            [("b".to_owned(), "x|y".to_owned())].into_iter().collect();
        assert_eq!(
            update_constraints("{a}/{b}/{a}/{c,z}", &rule, &global),
            "{a,[0-9]+}/{b,x|y}/{a}/{c,z}"
        );
    }

    #[test]
    fn repetition_is_periodic() {
        assert!(is_periodic(&format!("a{}", ".gz".repeat(20)), 20));
        assert!(!is_periodic("a.gz.gz", 20));
    }
}
