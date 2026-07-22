//! Rust: the preamble of rust-script scripts, as Snakemake's `RustScript` writes it: the
//! object as JSON, a struct made from it by `json_typegen`, and iteration and indexing of the
//! positional items.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::number::float_repr;

use super::data::{Data, Named};
use super::{Context, json_string};

pub type Encoded = Result<String, String>;

/// A value as JSON, as Python's `json.dumps` writes it; objects that it cannot write are the
/// `TypeError` that the job gets in Snakemake.
fn json(data: &Data) -> Encoded {
    Ok(match data {
        // numpy's `float64` is a float for `json.dumps`, its other scalars are not.
        Data::Scalar { kind, value } if kind == "float64" => json(value)?,
        Data::None => "null".into(),
        Data::Bool(true) => "true".into(),
        Data::Bool(false) => "false".into(),
        Data::Int(digits) => digits.clone(),
        Data::Float(value) if value.is_nan() => "NaN".into(),
        Data::Float(value) if value.is_infinite() => if *value > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into(),
        Data::Float(value) => float_repr(*value),
        Data::Str(text) => json_string(text),
        Data::List(items) | Data::Tuple(items) => list(items)?,
        Data::Named(named) => list(&named.values)?,
        Data::Dict(entries) => {
            let mut parts = Vec::new();
            for (key, value) in entries {
                let key = match key {
                    Data::Str(text) => text.clone(),
                    Data::None => "null".into(),
                    Data::Bool(flag) => flag.to_string(),
                    Data::Int(_) | Data::Float(_) => json(key)?,
                    other => {
                        return Err(format!(
                            "keys must be str, int, float, bool or None, not {}",
                            type_name(other)
                        ));
                    }
                };
                parts.push(format!("{}: {}", json_string(&key), json(value)?));
            }
            format!("{{{}}}", parts.join(", "))
        }
        other => {
            return Err(format!(
                "Object of type {} is not JSON serializable",
                type_name(other)
            ));
        }
    })
}

fn list(items: &[Data]) -> Encoded {
    Ok(format!(
        "[{}]",
        items
            .iter()
            .map(json)
            .collect::<Result<Vec<_>, _>>()?
            .join(", ")
    ))
}

fn type_name(data: &Data) -> &str {
    match data {
        Data::Complex(..) => "complex",
        Data::Bytes(_) => "bytes",
        Data::Path(_) => "PosixPath",
        Data::Set(_) => "set",
        Data::Table { kind, .. } => kind.rsplit('.').next().unwrap_or(kind),
        Data::Opaque { repr, .. } => repr,
        Data::Scalar { kind, .. } => kind,
        _ => "object",
    }
}

/// Named items as an object: the unnamed ones in `positional`, the named ones by name.
fn named_entries(entries: &[(Option<String>, Data)]) -> Encoded {
    let positional: Vec<Data> = entries
        .iter()
        .filter(|(name, _)| name.is_none())
        .map(|(_, value)| value.clone())
        .collect();
    let mut parts = vec![format!("\"positional\": {}", list(&positional)?)];
    for (name, value) in entries {
        if let Some(name) = name {
            parts.push(format!("{}: {}", json_string(name), json(value)?));
        }
    }
    Ok(format!("{{{}}}", parts.join(", ")))
}

fn all_items(named: &Named) -> Vec<(Option<String>, Data)> {
    named.all_items()
}

fn named_items(items: Vec<(String, Data)>) -> Vec<(Option<String>, Data)> {
    items
        .into_iter()
        .map(|(name, value)| (Some(name), value))
        .collect()
}

/// The object as JSON.
fn object(context: &Context) -> Encoded {
    let config = match &context.config {
        Data::Dict(entries) => entries
            .iter()
            .map(|(key, value)| (Some(key.text()), value.clone()))
            .collect(),
        _ => Vec::new(),
    };
    let bench = context
        .bench_iteration
        .map_or_else(|| "null".to_owned(), |value| value.to_string());
    Ok(format!(
        "{{\"input\": {}, \"output\": {}, \"params\": {}, \"wildcards\": {}, \"threads\": {}, \
         \"resources\": {}, \"log\": {}, \"config\": {}, \"rulename\": {}, \
         \"bench_iteration\": {bench}, \"scriptdir\": {}}}",
        named_entries(&all_items(&context.input))?,
        named_entries(&all_items(&context.output))?,
        named_entries(&named_items(context.params.items()))?,
        named_entries(&named_items(context.wildcards.items()))?,
        context.threads,
        named_entries(&named_items(context.plain_resources()))?,
        named_entries(&all_items(&context.log))?,
        named_entries(&config)?,
        json_string(&context.rule),
        json_string(&context.scriptdir),
    ))
}

/// The dependencies and features that Snakemake gives rust-script.
pub const DEPENDENCIES: &[&str] = &[
    "anyhow=1",
    "serde_json=1.0",
    "serde=1.0",
    "serde_derive=1.0",
    "lazy_static=1.4",
    "json_typegen=0.6",
    "gag=1",
];
pub const FEATURES: &str = "serde/derive";

/// The preamble of a Rust script.
pub fn preamble(context: &Context) -> Encoded {
    let json = object(context)?;
    Ok(format!(
        r####"
json_typegen::json_typegen!("Snakemake", r###"{json}"###, {{
    "/bench_iteration": {{
       "use_type": "Option<usize>"
    }},
    "/input/positional": {{
        "use_type": "Vec<String>"
    }},
    "/output/positional": {{
        "use_type": "Vec<String>"
    }},
    "/log/positional": {{
        "use_type": "Vec<String>"
    }},
    "/wildcards/positional": {{
        "use_type": "Vec<String>"
    }},
}});

pub struct Iter<'a, T>(std::slice::Iter<'a, T>);
impl<'a, T> Iterator for Iter<'a, T> {{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {{
        self.0.next()
    }}
}}
macro_rules! impl_iter {{
    ($($s:ty),+) => {{
        $(
            impl IntoIterator for $s {{
                type Item = String;
                type IntoIter = std::vec::IntoIter<Self::Item>;

                fn into_iter(self) -> Self::IntoIter {{
                    self.positional.into_iter()
                }}
            }}

            impl<'a> IntoIterator for &'a $s {{
                type Item = &'a String;
                type IntoIter = Iter<'a, String>;

                fn into_iter(self) -> Self::IntoIter {{
                    Iter(self.positional.as_slice().into_iter())
                }}
            }}
        )+
    }};
}}

macro_rules! impl_index {{
    ($($s:ty),+) => {{
        $(
        impl std::ops::Index<usize> for $s {{
            type Output = String;

            fn index(&self, index: usize) -> &Self::Output {{
                &self.positional[index]
            }}
        }}
        )+
    }}
}}


impl_iter!(Input, Output, Wildcards, Log);
impl_index!(Input, Output, Wildcards, Log);

impl Snakemake {{
    #[allow(dead_code)]
    fn redirect_stderr<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> anyhow::Result<gag::Redirect<std::fs::File>> {{
        let log = std::fs::OpenOptions::new()
            .truncate(true)
            .read(true)
            .create(true)
            .write(true)
            .open(path)?;
        Ok(gag::Redirect::stderr(log)?)
    }}

    #[allow(dead_code)]
    fn redirect_stdout<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> anyhow::Result<gag::Redirect<std::fs::File>> {{
        let log = std::fs::OpenOptions::new()
            .truncate(true)
            .read(true)
            .create(true)
            .write(true)
            .open(path)?;
        Ok(gag::Redirect::stdout(log)?)
    }}
}}

lazy_static::lazy_static! {{
    // https://github.com/rust-lang-nursery/lazy-static.rs/issues/153
    #[allow(non_upper_case_globals)]
    static ref snakemake: Snakemake = {{
        let s: Snakemake = serde_json::from_str(r###"{json}"###).expect("Failed parsing snakemake JSON");
        s
    }};
}}
// TODO include addendum, if any {{preamble_addendum}}
"####
    ))
}

/// The script as Snakemake's `RustScript.combine_preamble_and_source` joins it: the manifest
/// of the script, then the preamble, then the rest of the script, without its shebang.
pub fn combine(preamble: &str, source: &str) -> String {
    let (_, source) = super::shebang(source);
    let (manifest, rest) = manifest(source);
    format!(
        "{manifest}{}{rest}",
        preamble.trim_start_matches(['\r', '\n'])
    )
}

/// The cargo manifest at the start of a script: a `// cargo-deps:` line, or a crate doc
/// comment that holds a ```cargo block.
fn manifest(source: &str) -> (&str, &str) {
    let first = source.split_inclusive('\n').next().unwrap_or("");
    let trimmed = first.trim_start();
    if let Some(rest) = trimmed.strip_prefix("//")
        && rest
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("cargo-deps")
        && rest.trim_start()["cargo-deps".len()..]
            .trim_start()
            .starts_with(':')
    {
        return source.split_at(first.len());
    }
    let mut end = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if (trimmed.starts_with("//!") || trimmed.starts_with("///") || trimmed.starts_with("/*!"))
            && line.ends_with('\n')
        {
            end += line.len();
        } else {
            break;
        }
    }
    let comment = &source[..end];
    let mut opened = false;
    for line in comment.lines() {
        if !opened {
            opened = line
                .find("```")
                .is_some_and(|at| line[at + 3..].trim_start().starts_with("cargo"));
        } else if line.contains("```") {
            return source.split_at(end);
        }
    }
    ("", source)
}
