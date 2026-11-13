//! Writes Snakemake Bash associative arrays for named, positional, and scalar job values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::modules::shell_quote;

use super::Context;
use super::data::{Data, Named};

/// A Bash array from entries: lists become their items joined by spaces, as Snakemake cannot
/// nest arrays.
fn array(entries: &[(String, Data)]) -> String {
    let mut text = String::from("( ");
    for (key, value) in entries {
        let value = match value.items() {
            Some(items) => items.iter().map(Data::text).collect::<Vec<_>>().join(" "),
            None => value.text(),
        };
        text.push_str(&format!("[{}]={} ", shell_quote(key), shell_quote(&value)));
    }
    text.push(')');
    text
}

/// A `Namedlist` by name and by position, as `BashEncoder.encode_namedlist` gives it.
fn named_list(named: &Named) -> String {
    let mut entries = Vec::new();
    for (index, (name, value)) in named.all_items().into_iter().enumerate() {
        if let Some(name) = name {
            entries.push((name, value.clone()));
        }
        entries.push((index.to_string(), value));
    }
    array(&entries)
}

/// The arrays of the object, in the order of its attributes, as Snakemake declares them.
pub fn preamble(context: &Context) -> String {
    let config = match &context.config {
        Data::Dict(items) => items
            .iter()
            .map(|(key, value)| (key.text(), value.clone()))
            .collect(),
        _ => Vec::new(),
    };
    let bench = context.bench_iteration.map_or(Data::None, Data::int);
    let single = [
        ("_params_types".to_owned(), Data::List(Vec::new())),
        ("threads".to_owned(), Data::int(context.threads)),
        ("rule".to_owned(), Data::str(&context.rule)),
        ("bench_iteration".to_owned(), bench),
        ("scriptdir".to_owned(), Data::str(&context.scriptdir)),
    ];
    let arrays = [
        format!("snakemake_input={}", named_list(&context.input)),
        format!("snakemake_output={}", named_list(&context.output)),
        format!("snakemake_params={}", named_list(&context.params)),
        format!("snakemake_wildcards={}", named_list(&context.wildcards)),
        format!("snakemake_resources={}", named_list(&context.resources)),
        format!("snakemake_log={}", named_list(&context.log)),
        format!("snakemake_config={}", array(&config)),
        format!("snakemake={}", array(&single)),
    ];
    arrays
        .iter()
        .map(|array| format!("declare -A {array}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The script with the preamble after its shebang line, or after `#!/usr/bin/env bash` when it
/// has none, as `BashScript.combine_preamble_and_source` joins them.
pub fn combine(preamble: &str, source: &str) -> String {
    let (shebang, rest) = super::shebang(source);
    let shebang = if shebang.is_empty() {
        "#!/usr/bin/env bash"
    } else {
        shebang
    };
    [shebang, preamble, rest].join("\n")
}
