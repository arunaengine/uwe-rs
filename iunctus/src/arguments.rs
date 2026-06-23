//! The command line of `iunctus run`, `iunctus graph` and `iunctus config`: Nextflow's options,
//! and those that the other languages add.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use workflow_graph::{Language, Order};

/// The options of a command.
#[derive(Debug, Default, PartialEq)]
pub struct Arguments {
    /// The workflow file or project directory.
    pub target: Option<PathBuf>,
    /// `-lang name`; else the language is found from the target.
    pub language: Option<Language>,
    pub stub: bool,
    pub store: Option<PathBuf>,
    /// `-profile a,b`
    pub profiles: Option<Vec<String>>,
    /// `-c file`, `-configfile file`
    pub configs: Vec<PathBuf>,
    /// `--name value` and `--config name=value`, in the order given.
    pub params: Vec<(String, String)>,
    /// `-inputs file` or `-params-file file`: a CWL job file or a params file.
    pub inputs: Option<PathBuf>,
    /// `-entry name` and the arguments after the target, like Snakemake targets.
    pub targets: Vec<String>,
    /// `-executor simulator|local`; `None` for the default.
    pub executor: Option<String>,
    /// `-graph file`: where to write the execution graph.
    pub graph: Option<PathBuf>,
    /// `-format json|dot|summary` of `iunctus graph`.
    pub format: Option<String>,
    /// `-max-pending n`: how many tasks may be pending at once.
    pub max_pending: Option<usize>,
    /// `-order asked|reversed|shuffled[:seed]`: the order in which pending tasks finish.
    pub order: Option<Order>,
    /// `-default-image image`: the container image of tasks that name none, for real executors.
    pub default_image: Option<String>,
    /// `-python-image image`: the image of the Python container of decision 19, with the
    /// Python packages that the workflow imports.
    pub python_image: Option<String>,
}

/// Reads the options after the command name.
pub fn parse(arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        index += 1;
        let mut value = || {
            index += 1;
            arguments
                .get(index - 1)
                .cloned()
                .ok_or(format!("the option `{argument}` needs a value"))
        };
        match argument.as_str() {
            "-stub" | "-stub-run" => parsed.stub = true,
            "-store" => {
                let directory = value()?;
                parsed.store = Some(std::path::absolute(&directory).unwrap_or(directory.into()));
            }
            "-profile" => {
                // Groovy's `tokenize(',')`: empty names are dropped, spaces are kept.
                let names = value()?;
                let names = names.split(',').filter(|name| !name.is_empty());
                parsed.profiles = Some(names.map(str::to_owned).collect());
            }
            "-c" | "-config" => {
                let files = value()?;
                parsed.configs.extend(files.split(',').map(PathBuf::from));
            }
            "-configfile" | "--configfile" => parsed.configs.push(value()?.into()),
            "-inputs" | "-params-file" => parsed.inputs = Some(value()?.into()),
            "-entry" => parsed.targets.push(value()?),
            "-lang" => {
                let name = value()?;
                parsed.language = Some(language(&name).ok_or(format!(
                    "unknown language `{name}`; use nextflow, snakemake, cwl or argo"
                ))?);
            }
            "-executor" => parsed.executor = Some(value()?),
            "-graph" => parsed.graph = Some(value()?.into()),
            "-format" => parsed.format = Some(value()?),
            "-default-image" => parsed.default_image = Some(value()?),
            "-python-image" => parsed.python_image = Some(value()?),
            "-max-pending" => {
                let number = value()?;
                parsed.max_pending = Some(number.parse().ok().filter(|n| *n > 0).ok_or(
                    format!("`-max-pending` needs a number above zero, not `{number}`"),
                )?);
            }
            "-order" => {
                let name = value()?;
                parsed.order = Some(order(&name).ok_or(format!(
                    "unknown order `{name}`; use asked, reversed or shuffled[:<seed>]"
                ))?);
            }
            // Snakemake's `--config a=1 b=2` sets several params.
            "--config"
                if arguments
                    .get(index)
                    .is_some_and(|next| setting(next).is_some()) =>
            {
                while let Some((name, text)) = arguments.get(index).and_then(|next| setting(next)) {
                    parsed.params.push((name.to_owned(), text.to_owned()));
                    index += 1;
                }
            }
            param if param.starts_with("--") => {
                let (name, text) = match param[2..].split_once('=') {
                    Some((name, text)) => (name.to_owned(), text.to_owned()),
                    None => {
                        let next = arguments.get(index).filter(|next| takes(next));
                        match next {
                            Some(next) => {
                                index += 1;
                                (param[2..].to_owned(), next.clone())
                            }
                            None => (param[2..].to_owned(), "true".into()),
                        }
                    }
                };
                match setting(&text).filter(|_| name == "config") {
                    Some((name, text)) => parsed.params.push((name.to_owned(), text.to_owned())),
                    None => parsed.params.push((name, text)),
                }
            }
            other if !other.starts_with('-') => match parsed.target {
                None => parsed.target = Some(PathBuf::from(other)),
                Some(_) => parsed.targets.push(other.to_owned()),
            },
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(parsed)
}

/// A language by its name, as `-lang` takes it.
fn language(name: &str) -> Option<Language> {
    Some(match name.to_ascii_lowercase().as_str() {
        "nextflow" | "nf" => Language::Nextflow,
        "snakemake" | "smk" => Language::Snakemake,
        "cwl" => Language::Cwl,
        "argo" => Language::Argo,
        _ => return None,
    })
}

/// An order by its name, as `-order` takes it: `asked`, `reversed`, `shuffled` or
/// `shuffled:<seed>`; the seed is 0 when not given.
fn order(name: &str) -> Option<Order> {
    match name.split_once(':') {
        None if name == "asked" => Some(Order::AsAsked),
        None if name == "reversed" => Some(Order::Reversed),
        None if name == "shuffled" => Some(Order::Shuffled { seed: 0 }),
        Some(("shuffled", seed)) => seed.parse().ok().map(|seed| Order::Shuffled { seed }),
        _ => None,
    }
}

/// `name=value`, as Snakemake's `--config` takes it.
fn setting(text: &str) -> Option<(&str, &str)> {
    text.split_once('=')
        .filter(|(name, _)| !name.is_empty() && !name.starts_with('-'))
}

/// Whether the argument after `--name` is its value: it does not start with `-`, or it is a
/// number, contains a space or is one character long. An empty argument is not taken.
fn takes(next: &str) -> bool {
    !next.is_empty()
        && (!next.starts_with('-')
            || next.parse::<f64>().is_ok()
            || next.contains(' ')
            || next.chars().count() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(arguments: &[&str]) -> Arguments {
        let arguments: Vec<String> = arguments.iter().map(|text| text.to_string()).collect();
        parse(&arguments).expect("valid options")
    }

    #[test]
    fn params_take_their_values() {
        let parsed = parse_all(&["main.nf", "--outdir", "out", "--skip", "--n", "-1", "-stub"]);
        assert_eq!(parsed.target, Some(PathBuf::from("main.nf")));
        let params = [("outdir", "out"), ("skip", "true"), ("n", "-1")];
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()))
            .collect();
        assert_eq!(parsed.params, params);
        assert!(parsed.stub);
    }

    #[test]
    fn snakemake_options() {
        let parsed = parse_all(&[
            "Snakefile",
            "all",
            "--config",
            "samples=s.tsv",
            "n=2",
            "-configfile",
            "config.yaml",
            "-lang",
            "snakemake",
        ]);
        assert_eq!(parsed.targets, ["all"]);
        assert_eq!(
            parsed.params,
            [
                ("samples".to_owned(), "s.tsv".to_owned()),
                ("n".to_owned(), "2".to_owned())
            ]
        );
        assert_eq!(parsed.configs, [PathBuf::from("config.yaml")]);
        assert_eq!(parsed.language, Some(Language::Snakemake));
    }

    #[test]
    fn a_config_param_without_a_setting_is_a_param() {
        let parsed = parse_all(&["main.nf", "--config", "file.txt"]);
        assert_eq!(
            parsed.params,
            [("config".to_owned(), "file.txt".to_owned())]
        );
    }

    #[test]
    fn orders_and_pending_limits() {
        let parsed = parse_all(&["main.nf", "-max-pending", "4", "-order", "shuffled:7"]);
        assert_eq!(parsed.default_image, None);
        let parsed = parse_all(&["Snakefile", "-python-image", "python:3.12"]);
        assert_eq!(parsed.python_image.as_deref(), Some("python:3.12"));
        let parsed = parse_all(&["main.nf", "-max-pending", "4", "-order", "shuffled:7"]);
        assert_eq!(parsed.max_pending, Some(4));
        assert_eq!(parsed.order, Some(Order::Shuffled { seed: 7 }));
        assert_eq!(order("reversed"), Some(Order::Reversed));
        assert_eq!(order("shuffled"), Some(Order::Shuffled { seed: 0 }));
        for wrong in [
            ["-order", "random"],
            ["-order", "shuffled:x"],
            ["-max-pending", "0"],
        ] {
            let arguments = wrong.map(str::to_owned);
            assert!(parse(&arguments).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn unknown_languages_and_options_are_errors() {
        let arguments = ["-lang".to_owned(), "wdl".to_owned()];
        assert!(parse(&arguments).is_err());
        assert!(parse(&["-x".to_owned()]).is_err());
    }
}
