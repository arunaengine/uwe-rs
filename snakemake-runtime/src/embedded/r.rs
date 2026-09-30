//! R: the S4 `snakemake` object of R scripts and R Markdown documents, as Snakemake's
//! `REncoder` and `RScript.generate_preamble` write it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::format::quote;
use python_evaluator::number::float_repr;

use super::Context;
use super::data::{Data, Named};

/// A value that R cannot take, with the message of Snakemake's `ValueError`.
pub type Encoded = Result<String, String>;

fn numeric(data: &Data) -> Encoded {
    Ok(match data {
        Data::None => "as.numeric(NA)".into(),
        Data::Bool(flag) => format!("{}L", i64::from(*flag)),
        Data::Int(digits) => format!("{digits}L"),
        Data::Float(value) if value.is_nan() => "NaN".into(),
        Data::Float(value) if value.is_infinite() => {
            if *value > 0.0 { "Inf" } else { "-Inf" }.into()
        }
        Data::Float(value) => float_repr(*value),
        Data::Complex(real, imag) => format!(
            "{}+{}i",
            numeric(&Data::Float(*real))?,
            numeric(&Data::Float(*imag))?
        ),
        _ => return Err("Value is not a proper number".into()),
    })
}

/// A value as R code, as `REncoder.encode_value` writes it.
pub fn value(data: &Data) -> Encoded {
    match data {
        // `REncoder` takes numpy scalars by their `item()`.
        Data::Scalar { value, .. } => self::value(value),
        Data::None => Ok("NULL".into()),
        Data::Bool(true) => Ok("TRUE".into()),
        Data::Bool(false) => Ok("FALSE".into()),
        Data::Int(_) | Data::Float(_) | Data::Complex(..) => numeric(data),
        Data::Str(text) | Data::Path(text) => Ok(quote(text)),
        Data::Dict(entries) => dict(entries),
        _ => match data.items() {
            Some(items) => list(&items),
            None => Err(format!(
                "Unsupported value for conversion into R: {}",
                data.text()
            )),
        },
    }
}

/// A vector when all items are numbers, all texts or all bytes, else a list.
fn list(items: &[Data]) -> Encoded {
    let homogeneous = items.is_empty()
        || items.iter().all(Data::is_number)
        || items.iter().all(|item| matches!(item, Data::Str(_)))
        || items.iter().all(|item| matches!(item, Data::Bytes(_)));
    let encoded = items.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "{}({})",
        if homogeneous { "c" } else { "list" },
        encoded.join(", ")
    ))
}

fn entries(entries: &[(String, Data)]) -> Encoded {
    Ok(entries
        .iter()
        .map(|(name, item)| Ok(format!("\"{name}\" = {}", value(item)?)))
        .collect::<Result<Vec<_>, String>>()?
        .join(", "))
}

fn dict(items: &[(Data, Data)]) -> Encoded {
    let named: Vec<(String, Data)> = items
        .iter()
        .map(|(key, item)| (key.text(), item.clone()))
        .collect();
    Ok(format!("list({})", entries(&named)?))
}

/// A `Namedlist`: every item by position, then the named ones by name.
pub fn named_list(named: &Named) -> Encoded {
    let positional = named
        .values
        .iter()
        .map(value)
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    list_with(&positional, &entries(&named.items())?)
}

fn list_with(positional: &str, named: &str) -> Encoded {
    let mut source = String::from("list(");
    source.push_str(positional);
    if !named.is_empty() {
        source.push_str(", ");
        source.push_str(named);
    }
    source.push(')');
    Ok(source)
}

/// The resources without `_cores` and `_nodes`, which Snakemake encodes as a dict given to its
/// `Namedlist` encoder: the names by position, then the values by name.
pub fn resources(resources: &[(String, Data)]) -> Encoded {
    let positional = resources
        .iter()
        .map(|(name, _)| quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    list_with(&positional, &entries(resources)?)
}

/// The class and the object, as Snakemake's R preamble makes them. Scripts and notebooks have
/// a line for an addendum at the end; R Markdown documents have none.
pub fn object(context: &Context, addendum: Option<&str>) -> Encoded {
    let config = match &context.config {
        Data::Dict(items) => dict(items)?,
        other => value(other)?,
    };
    let bench = match context.bench_iteration {
        Some(iteration) => format!("{iteration}L"),
        None => "as.numeric(NA)".into(),
    };
    let addendum = addendum.map_or_else(String::new, |addendum| format!("{addendum}\n"));
    Ok(format!(
        r#"
######## snakemake preamble start (automatically inserted, do not edit) ########
library(methods)
Snakemake <- setClass(
    "Snakemake",
    slots = c(
        input = "list",
        output = "list",
        params = "list",
        wildcards = "list",
        threads = "numeric",
        log = "list",
        resources = "list",
        config = "list",
        rule = "character",
        bench_iteration = "numeric",
        scriptdir = "character",
        source = "function"
    )
)
snakemake <- Snakemake(
    input = {},
    output = {},
    params = {},
    wildcards = {},
    threads = {},
    log = {},
    resources = {},
    config = {config},
    rule = {},
    bench_iteration = {bench},
    scriptdir = {},
    source = function(...){{
        old_wd <- getwd()
        on.exit(setwd(old_wd), add = TRUE)

        is_url <- grepl("^https?://", snakemake@scriptdir)
        file <- ifelse(is_url, file.path(snakemake@scriptdir, ...), ...)
        if (!is_url) setwd(snakemake@scriptdir)
        source(file)
    }}
)
{addendum}
######## snakemake preamble end #########
"#,
        named_list(&context.input)?,
        named_list(&context.output)?,
        named_list(&context.params)?,
        named_list(&context.wildcards)?,
        context.threads,
        named_list(&context.log)?,
        resources(&context.plain_resources())?,
        quote(&context.rule),
        quote(&context.scriptdir),
    ))
}

/// The indentation of the chunk of the preamble, which Snakemake's `textwrap.dedent` keeps, as
/// the lines of the preamble have none.
const CHUNK_INDENT: &str = "            ";

/// The R Markdown document with the preamble in a chunk right after its YAML header, as
/// Snakemake's `RMarkdown.write_script` inserts it.
pub fn markdown(source: &str, preamble: &str) -> Result<String, String> {
    let position = source
        .match_indices("---\n")
        .nth(1)
        .map(|(start, _)| start + 3)
        .ok_or_else(|| {
            "an R Markdown script needs a YAML header between two `---` lines".to_owned()
        })?;
    let chunk = format!(
        "\n{CHUNK_INDENT}```{{r, echo=FALSE, message=FALSE, warning=FALSE}}\n{preamble}\n{CHUNK_INDENT}```\n"
    );
    Ok(format!(
        "{}{chunk}{}",
        &source[..position],
        &source[position..]
    ))
}

#[cfg(test)]
mod tests {
    use super::{list, value};
    use crate::embedded::data::Data;

    #[test]
    fn serializes_homogeneous_lists() {
        let numbers = [Data::Bool(true), Data::int(2), Data::Float(1.5)];
        assert_eq!(list(&numbers).as_deref(), Ok("c(TRUE, 2L, 1.5)"));
        let mixed = [Data::Bool(true), Data::int(123), Data::str("abc")];
        assert_eq!(list(&mixed).as_deref(), Ok("list(TRUE, 123L, 'abc')"));
        assert_eq!(list(&[]).as_deref(), Ok("c()"));
    }

    #[test]
    fn rejects_unrepresentable_objects() {
        let function = Data::Opaque {
            repr: "<function f>".into(),
            text: "<function f>".into(),
        };
        assert_eq!(
            value(&function),
            Err("Unsupported value for conversion into R: <function f>".into())
        );
    }
}
