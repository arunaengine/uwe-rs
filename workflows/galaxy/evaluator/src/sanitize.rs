//! Galaxy's sanitizing of text and select values: the default rules and a param's `<sanitizer>`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use galaxy_parser::tool::{Param, Rules};

const LETTERS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &str = "0123456789";
const PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
const WHITESPACE: &str = " \t\n\r\u{b}\u{c}";
/// Galaxy's `valid_chars` besides letters and digits.
const DEFAULT_EXTRA: &str = " -=_.()/+*^,:?!";
/// Galaxy's `mapped_chars`.
const DEFAULT_MAPPING: &[(&str, &str)] = &[
    (">", "__gt__"),
    ("<", "__lt__"),
    ("'", "__sq__"),
    ("\"", "__dq__"),
    ("[", "__ob__"),
    ("]", "__cb__"),
    ("{", "__oc__"),
    ("}", "__cc__"),
    ("@", "__at__"),
    ("\n", "__cn__"),
    ("\r", "__cr__"),
    ("\t", "__tc__"),
    ("#", "__pd__"),
];

/// The characters kept, the characters replaced by text, and the replacement of all others.
#[derive(Clone, Debug, PartialEq)]
pub struct Sanitizer {
    valid: Vec<char>,
    mapping: Vec<(String, String)>,
    invalid: String,
}

impl Sanitizer {
    /// The sanitizer of a param, or none when the tool or the param turns sanitizing off.
    pub fn of(param: &Param, tool_sanitizes: bool) -> Option<Sanitizer> {
        if !tool_sanitizes {
            return None;
        }
        let Some(own) = &param.sanitizer else {
            return Some(Sanitizer::default());
        };
        if !own.sanitize {
            return None;
        }
        // Without <valid> or <mapping>, Galaxy's sanitizer starts from empty sets.
        Some(Sanitizer {
            valid: own.valid.as_ref().map(valid).unwrap_or_default(),
            mapping: own.mapping.as_ref().map(mapping).unwrap_or_default(),
            invalid: own.invalid.clone(),
        })
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            if self.valid.contains(&c) {
                out.push(c);
            } else if let Some((_, target)) =
                (self.mapping.iter()).find(|(source, _)| source.chars().eq([c]))
            {
                out.push_str(target);
            } else {
                out.push_str(&self.invalid);
            }
        }
        out
    }
}

impl Default for Sanitizer {
    fn default() -> Sanitizer {
        Sanitizer {
            valid: preset_chars("default"),
            mapping: preset_mapping("default"),
            invalid: "X".to_owned(),
        }
    }
}

fn valid(rules: &Rules) -> Vec<char> {
    let mut chars = preset_chars(&rules.initial);
    for action in &rules.actions {
        let mut given = preset_chars(action.preset.as_deref().unwrap_or("none"));
        given.extend(action.value.as_deref().unwrap_or_default().chars());
        for c in given {
            if action.add && !chars.contains(&c) {
                chars.push(c);
            } else if !action.add {
                chars.retain(|known| *known != c);
            }
        }
    }
    chars
}

fn mapping(rules: &Rules) -> Vec<(String, String)> {
    let mut mapping = preset_mapping(&rules.initial);
    for action in &rules.actions {
        let preset = preset_mapping(action.preset.as_deref().unwrap_or("none"));
        if action.add {
            for (source, target) in preset {
                set(&mut mapping, source, target);
            }
            if let (Some(source), Some(target)) = (&action.source, &action.target) {
                set(&mut mapping, source.clone(), target.clone());
            }
        } else {
            mapping.retain(|(source, _)| {
                !preset.iter().any(|(known, _)| known == source)
                    && action.source.as_ref() != Some(source)
            });
        }
    }
    mapping
}

fn set(mapping: &mut Vec<(String, String)>, source: String, target: String) {
    match mapping.iter_mut().find(|(known, _)| *known == source) {
        Some(entry) => entry.1 = target,
        None => mapping.push((source, target)),
    }
}

/// The characters of preset names like `default`, `none` or `string.printable`, separated by
/// commas, as Galaxy's `get_valid_by_name`.
fn preset_chars(names: &str) -> Vec<char> {
    let mut chars: Vec<char> = Vec::new();
    for name in names.split(',') {
        let name = name.split(';').next().unwrap_or_default().trim();
        let lower = &LETTERS[..26];
        let upper = &LETTERS[26..];
        let text: String = match name.strip_prefix("string.") {
            Some("ascii_letters" | "letters") => LETTERS.to_owned(),
            Some("ascii_lowercase" | "lowercase") => lower.to_owned(),
            Some("ascii_uppercase" | "uppercase") => upper.to_owned(),
            Some("digits") => DIGITS.to_owned(),
            Some("hexdigits") => format!("{DIGITS}abcdefABCDEF"),
            Some("octdigits") => DIGITS[..8].to_owned(),
            Some("punctuation") => PUNCTUATION.to_owned(),
            Some("whitespace") => WHITESPACE.to_owned(),
            Some("printable") => format!("{DIGITS}{LETTERS}{PUNCTUATION}{WHITESPACE}"),
            Some(_) => String::new(),
            None if name == "default" => format!("{LETTERS}{DIGITS}{DEFAULT_EXTRA}"),
            None => String::new(),
        };
        for c in text.chars() {
            if !chars.contains(&c) {
                chars.push(c);
            }
        }
    }
    chars
}

fn preset_mapping(names: &str) -> Vec<(String, String)> {
    let mut mapping = Vec::new();
    for name in names.split(',') {
        if name.trim() == "default" {
            for (source, target) in DEFAULT_MAPPING {
                set(&mut mapping, (*source).to_owned(), (*target).to_owned());
            }
        }
    }
    mapping
}

#[cfg(test)]
mod tests {
    use galaxy_parser::tool::{Action, Sanitizer as Declared};

    use super::*;

    fn declared(sanitizer: Declared) -> Option<Sanitizer> {
        let xml = "<tool id=\"t\"><inputs><param name=\"p\" type=\"text\"/></inputs></tool>";
        let tool =
            galaxy_parser::tool::parse(xml, "t.xml", &mut |_| unreachable!()).expect("a tool");
        let galaxy_parser::tool::Input::Param(mut param) = tool.inputs[0].clone() else {
            unreachable!("one param");
        };
        param.sanitizer = Some(sanitizer);
        Sanitizer::of(&param, true)
    }

    #[test]
    fn default_maps_quotes() {
        let sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.apply("a b,c_d"), "a b,c_d");
        assert_eq!(
            sanitizer.apply("it's \"x\" @home #1"),
            "it__sq__s __dq__x__dq__ __at__home __pd__1"
        );
        assert_eq!(sanitizer.apply("a;b|c$d&e`f"), "aXbXcXdXeXf");
        assert_eq!(sanitizer.apply("tab\tline\n"), "tab__tc__line__cn__");
    }

    #[test]
    fn declared_rules_apply() {
        let letters = declared(Declared {
            sanitize: true,
            invalid: String::new(),
            valid: Some(Rules {
                initial: "string.ascii_letters".to_owned(),
                actions: Vec::new(),
            }),
            mapping: None,
        })
        .expect("sanitizes");
        assert_eq!(
            letters.apply("a1 'b'"),
            "ab",
            "no mapping without <mapping>"
        );
        let printable = declared(Declared {
            sanitize: true,
            invalid: "X".to_owned(),
            valid: Some(Rules {
                initial: "none".to_owned(),
                actions: vec![
                    Action {
                        add: true,
                        preset: Some("string.printable".to_owned()),
                        ..Action::default()
                    },
                    Action {
                        add: false,
                        value: Some("\"".to_owned()),
                        ..Action::default()
                    },
                ],
            }),
            mapping: Some(Rules {
                initial: "none".to_owned(),
                actions: vec![Action {
                    add: true,
                    source: Some("\"".to_owned()),
                    target: Some("\\\"".to_owned()),
                    ..Action::default()
                }],
            }),
        })
        .expect("sanitizes");
        assert_eq!(printable.apply("say \"$x\";"), "say \\\"$x\\\";");
        let off = declared(Declared {
            sanitize: false,
            invalid: "X".to_owned(),
            valid: None,
            mapping: None,
        });
        assert!(off.is_none(), "sanitize=\"False\" keeps the value");
    }
}
