//! Julia: the `snakemake` struct of Julia scripts, as Snakemake's `JuliaEncoder` and
//! `JuliaScript.get_preamble` write it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::data::{Data, Named};
use super::{Context, json_string};

pub type Encoded = Result<String, String>;

/// A value as Julia code, as `JuliaEncoder.encode_value` writes it.
pub fn value(data: &Data) -> Encoded {
    match data {
        // `JuliaEncoder` writes numpy numbers by `str()`; numpy's booleans it cannot take.
        Data::Scalar { kind, .. } if kind == "bool_" => Err(format!(
            "Unsupported value for conversion into Julia: {}",
            data.text()
        )),
        Data::Scalar { value, .. } => self::value(value),
        Data::None => Ok("nothing".into()),
        Data::Str(text) | Data::Path(text) => Ok(json_string(text)),
        Data::Dict(entries) => dict(entries),
        Data::Bool(true) => Ok("true".into()),
        Data::Bool(false) => Ok("false".into()),
        Data::Int(_) | Data::Float(_) => Ok(data.text()),
        _ => match data.items() {
            Some(items) => Ok(format!(
                "[{}]",
                items
                    .iter()
                    .map(value)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )),
            None => Err(format!(
                "Unsupported value for conversion into Julia: {}",
                data.text()
            )),
        },
    }
}

fn entries(entries: &[(Data, Data)]) -> Encoded {
    Ok(entries
        .iter()
        .map(|(name, item)| Ok(format!("{} => {}", value(name)?, value(item)?)))
        .collect::<Result<Vec<_>, String>>()?
        .join(", "))
}

fn dict(items: &[(Data, Data)]) -> Encoded {
    Ok(format!("Dict({})", entries(items)?))
}

fn positional(values: &[Data]) -> Encoded {
    let mut encoded = String::new();
    for (index, item) in values.iter().enumerate() {
        encoded.push_str(&format!("{} => {}, ", index + 1, value(item)?));
    }
    Ok(encoded)
}

/// A `Namedlist`: every item by its position from 1, then the named ones by name.
fn named_list(named: &Named) -> Encoded {
    let items: Vec<(Data, Data)> = named
        .items()
        .into_iter()
        .map(|(name, item)| (Data::Str(name), item))
        .collect();
    Ok(format!(
        "Dict({}{})",
        positional(&named.values)?,
        entries(&items)?
    ))
}

/// The resources without `_cores` and `_nodes`, as a dict given to the `Namedlist` encoder:
/// the names by position, then the values by name.
fn resources(resources: &[(String, Data)]) -> Encoded {
    let names: Vec<Data> = resources.iter().map(|(name, _)| Data::str(name)).collect();
    let items: Vec<(Data, Data)> = resources
        .iter()
        .map(|(name, item)| (Data::str(name), item.clone()))
        .collect();
    Ok(format!("Dict({}{})", positional(&names)?, entries(&items)?))
}

/// The struct and the object, as Snakemake's Julia preamble makes them.
pub fn preamble(context: &Context) -> Encoded {
    let config = match &context.config {
        Data::Dict(items) => dict(items)?,
        other => value(other)?,
    };
    let bench = match context.bench_iteration {
        Some(iteration) => iteration.to_string(),
        None => "nothing".into(),
    };
    Ok(format!(
        r#"
######## snakemake preamble start (automatically inserted, do not edit) ########
struct Snakemake
    input::Dict
    output::Dict
    params::Dict
    wildcards::Dict
    threads::Int64
    log::Dict
    resources::Dict
    config::Dict
    rule::String
    bench_iteration
    scriptdir::String
    #source::Any
end
snakemake = Snakemake(
    {}, #input::Dict
    {}, #output::Dict
    {}, #params::Dict
    {}, #wildcards::Dict
    {}, #threads::Int64
    {}, #log::Dict
    {}, #resources::Dict
    {config}, #config::Dict
    {}, #rule::String
    {bench}, #bench_iteration::Int64
    {}, #scriptdir::String
    #, #source::Any
)
######## snakemake preamble end #########
"#,
        named_list(&context.input)?,
        named_list(&context.output)?,
        named_list(&context.params)?,
        named_list(&context.wildcards)?,
        context.threads,
        named_list(&context.log)?,
        resources(&context.plain_resources())?,
        json_string(&context.rule),
        json_string(&context.scriptdir),
    ))
}
