//! Reads the words of a command line into the options of a command: the POSIX spellings, the
//! values of each option, and the engine options that uwe refuses with a hint.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use workflow_graph::render::{Direction, Format, View};
use workflow_graph::{Language, Order};

use super::options::{COMMANDS, OPTIONS, WORKFLOW};
use super::{Arguments, Resume};

/// Reads the options after the command name.
pub fn parse(arguments: &[String]) -> Result<Arguments, String> {
    parse_for(None, arguments)
}

pub fn parse_command(command: &str, arguments: &[String]) -> Result<Arguments, String> {
    if !COMMANDS.contains(&command) {
        return Err(format!("unknown command `{command}`"));
    }
    let parsed = parse_for(Some(command), arguments)?;
    if matches!(command, "version" | "presets") && parsed.target.is_some() {
        return Err(format!("`uwe {command}` takes no target"));
    }
    Ok(parsed)
}

fn parse_for(command: Option<&str>, arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut parameter_options = Vec::new();
    let (words, delimiter) = posix(command, arguments);
    let arguments = words.as_slice();
    // After the `--` of a workflow command every `--name` is a workflow parameter.
    let mut after = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        index += 1;
        if command == Some("record") && argument == "--" {
            parsed.command_line = arguments[index..].to_vec();
            break;
        }
        if delimiter == Some(index - 1) {
            after = true;
            continue;
        }
        // After `--` each `--name value` is a workflow parameter as written, also `--config`,
        // and no engine option of any language.
        if after {
            match argument.strip_prefix("--").filter(|name| !name.is_empty()) {
                Some(name) => {
                    let (name, text) = match name.split_once('=') {
                        Some((name, text)) => (name.to_owned(), text.to_owned()),
                        None => match arguments.get(index).filter(|next| takes(next)) {
                            Some(next) => {
                                index += 1;
                                (name.to_owned(), next.clone())
                            }
                            None => (name.to_owned(), "true".into()),
                        },
                    };
                    parsed.params.push((name, text));
                }
                None if !argument.starts_with('-') => match parsed.target {
                    None => parsed.target = Some(PathBuf::from(argument)),
                    Some(_) => parsed.targets.push(argument.clone()),
                },
                None => {
                    return Err(format!(
                        "after `--` come workflow parameters like `--name value`, not `{argument}`"
                    ));
                }
            }
            continue;
        }
        if let Some(command) = command {
            let option = OPTIONS.iter().find(|(names, _)| {
                names
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .any(|name| name == argument)
            });
            let allowed = option
                .map(|(_, commands)| *commands)
                .or_else(|| argument.starts_with("--").then_some(WORKFLOW));
            if let Some(allowed) = allowed
                && !allowed.contains(&command)
            {
                return Err(format!(
                    "option `{argument}` does not apply to `uwe {command}`; use `uwe {command} -help`"
                ));
            }
        }
        let mut value = || {
            index += 1;
            arguments
                .get(index - 1)
                .cloned()
                .ok_or(format!("the option `{argument}` needs a value"))
        };
        match argument.as_str() {
            "-help" => parsed.help = true,
            "-version" => parsed.version = true,
            "--version" => parsed.version = true,
            "-json" => parsed.json = true,
            "-tools" => parsed.tools.push(value()?.into()),
            "-workflows" => parsed.workflows.push(value()?.into()),
            "-token" => parsed.token = Some(value()?),
            "-open" => parsed.open = true,
            "-v" | "-verbose" => parsed.verbose = true,
            "-quiet" => parsed.quiet = true,
            "-latest" => parsed.latest = true,
            "-r" | "-revision" => {
                let revision = value()?;
                if revision.is_empty() || revision.starts_with('-') {
                    return Err(format!("the option `{argument}` needs a revision"));
                }
                parsed.revision = Some(revision);
            }
            "-stub" | "-stub-run" => parsed.stub = true,
            "-keep-going" => parsed.keep_going = true,
            "-strict-provenance" => parsed.strict_provenance = true,
            "-reason" => {
                let reason = value()?;
                if reason.trim().is_empty() || reason.starts_with('-') {
                    return Err("-reason needs nonempty explanatory text".into());
                }
                parsed.reason = Some(reason);
            }
            "-forceall" => parsed.forceall = true,
            "-rerun-triggers" => {
                let triggers = value()?;
                let names: Vec<String> = triggers.split(',').map(str::to_owned).collect();
                if names.iter().any(|name| {
                    !["mtime", "params", "input", "software-env", "code"].contains(&name.as_str())
                }) {
                    return Err(
                        "-rerun-triggers expects mtime,params,input,software-env,code or a subset"
                            .into(),
                    );
                }
                parsed.rerun_triggers = Some(names);
            }
            "-until" | "-omit-from" | "-forcerun" => {
                let names = value()?;
                if names.starts_with('-') || names.split(',').any(str::is_empty) {
                    return Err(format!("the option `{argument}` needs rules or files"));
                }
                let targets = match argument.as_str() {
                    "-until" => &mut parsed.until,
                    "-omit-from" => &mut parsed.omit_from,
                    _ => &mut parsed.forcerun,
                };
                targets.extend(names.split(',').map(str::to_owned));
            }
            "-store" => {
                let place = value()?;
                parsed.store = Some(match storage::address::scheme(&place) {
                    Some(_) => place,
                    None => std::path::absolute(&place)
                        .map_or(place, |folder| folder.to_string_lossy().into_owned()),
                });
            }
            "-uwe-config" => parsed.config_file = Some(value()?.into()),
            "-head-image" => parsed.head_image = Some(value()?),
            "-name" => parsed.tool_name = Some(value()?),
            "-image" => parsed.tool_image = Some(value()?),
            "-head-cpus" => parsed.head_cpus = Some(value()?),
            "-head-memory" => parsed.head_memory = Some(value()?),
            "-volume-mount" => {
                let mounts = value()?;
                parsed
                    .volume_mounts
                    .extend(mounts.split(',').map(str::to_owned));
            }
            "-background" => parsed.background = true,
            "-fail-on-change" => parsed.fail_on_change = true,
            "-preset" => parsed.preset = Some(value()?),
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
            "-configfile" => parsed.configs.push(value()?.into()),
            "--configfile" => parsed.configs.push(value()?.into()),
            "-inputs" | "-params-file" => parsed.inputs = Some(value()?.into()),
            "-entry" => parsed.targets.push(value()?),
            "-lang" => {
                let name = value()?;
                parsed.language = Some(language(&name).ok_or(format!(
                    "unknown language `{name}`; use nextflow, snakemake, cwl, argo, wdl or wir"
                ))?);
            }
            "-executor" => parsed.executor = Some(value()?),
            "-graph" => parsed.graph = Some(value()?.into()),
            "-report" => parsed.report = Some(value()?.into()),
            "-output-dir" => parsed.output_dir = Some(value()?.into()),
            "-workdir" => parsed.workdir = Some(value()?.into()),
            "-tes-server" => parsed.tes_server = Some(value()?),
            "-slurm-container" => parsed.slurm_container = Some(value()?),
            "-slurm-options" => parsed.slurm_options = Some(value()?),
            "-kubeconfig" => parsed.kubeconfig = Some(value()?.into()),
            "-kubernetes-namespace" => parsed.kubernetes_namespace = Some(value()?),
            "-format" => {
                let name = value()?;
                let other = matches!(name.as_str(), "text" | "form" | "cwl" | "tool" | "both");
                if Format::parse(&name).is_none() && !other {
                    return Err(format!(
                        "unknown graph format `{name}`; use json, dot, summary or svg, json, \
                         text or form for `uwe inputs`, or cwl, tool or both for `uwe record`"
                    ));
                }
                parsed.format = Some(name);
            }
            "-check" => parsed.check = true,
            "-view" => {
                let name = value()?;
                parsed.view = Some(
                    View::parse(&name)
                        .ok_or(format!("unknown graph view `{name}`; use tasks or steps"))?,
                );
            }
            "-direction" => {
                let name = value()?;
                parsed.direction = Some(
                    Direction::parse(&name)
                        .ok_or(format!("unknown direction `{name}`; use down or right"))?,
                );
            }
            "-fold" => {
                let number = value()?;
                parsed.fold = Some(number.parse().map_err(|_| {
                    format!("`-fold` needs a number of tasks, like 5, not `{number}`")
                })?);
            }
            "-default-image" => parsed.default_image = Some(value()?),
            "-python-image" => parsed.python_image = Some(value()?),
            "-database" => parsed.database = Some(value()?.into()),
            "-output" => parsed.output = Some(value()?.into()),
            "-license" => parsed.license = Some(value()?),
            "-platform" => parsed.platform = Some(value()?),
            "-resume" => {
                // The run is optional: the next argument is taken only when it looks like a run.
                parsed.resume = Some(
                    match arguments.get(index).filter(|next| looks_like_run(next)) {
                        Some(run) => {
                            index += 1;
                            Resume::Run(run.clone())
                        }
                        None => Resume::Last,
                    },
                );
            }
            "-page" => parsed.page = true,
            "-keep-days" => {
                let number = value()?;
                parsed.keep_days = Some(number.parse().map_err(|_| {
                    format!("`-keep-days` needs a whole number of days, like 30, not `{number}`")
                })?);
            }
            "-keep-run" => parsed.keep_runs.push(value()?),
            "-keep-slower-than" => {
                let text = value()?;
                parsed.keep_slower_than = Some(duration(&text).ok_or(format!(
                    "`-keep-slower-than` needs a time like 90s, 10m, 2h or 1d, not `{text}`"
                ))?);
            }
            "-grace" => {
                let text = value()?;
                parsed.grace = Some(
                    duration(&text)
                        .ok_or(format!("`-grace` needs a time like 2h or 1d, not `{text}`"))?,
                );
            }
            "-remove" => parsed.remove = true,
            "-front" => parsed.front = true,
            "-no-network" => parsed.no_network = true,
            "-check-reproducibility" => {
                let text = value()?;
                parsed.check_reproducibility = Some(percent(&text).ok_or(format!(
                    "`-check-reproducibility` needs a share of the cached tasks from 0% to 100%, \
                     like 10%, not `{text}`"
                ))?);
            }
            "-no-cache" => parsed.no_cache = true,
            "-no-cache-steps" => {
                let names = value()?;
                if names.starts_with('-') || names.split(',').any(str::is_empty) {
                    return Err("the option `-no-cache-steps` needs step names".into());
                }
                parsed
                    .no_cache_steps
                    .extend(names.split(',').map(str::to_owned));
            }
            "-no-right-sizing" => parsed.no_right_sizing = true,
            "-no-grouping" => parsed.no_grouping = true,
            "-address" => {
                let text = value()?;
                parsed.address = Some(text.parse().map_err(|_| {
                    format!("`-address` needs an IP address like 127.0.0.1, not `{text}`")
                })?);
            }
            "-port" => {
                let text = value()?;
                parsed.port = Some(
                    text.parse()
                        .map_err(|_| format!("`-port` needs a port number, not `{text}`"))?,
                );
            }
            "-max-pending" => {
                let number = value()?;
                parsed.max_pending = Some(number.parse().ok().filter(|n| *n > 0).ok_or(
                    format!("`-max-pending` needs a number above zero, not `{number}`"),
                )?);
            }
            "-machine-retries" => {
                let number = value()?;
                parsed.machine_retries = Some(number.parse().map_err(|_| {
                    format!("`-machine-retries` needs a whole number from zero, not `{number}`")
                })?);
            }
            "-machine-retry-delay-ms" => {
                let number = value()?;
                parsed.retry_delay_ms =
                    Some(number.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                        format!(
                            "`-machine-retry-delay-ms` needs a number above zero, not `{number}`"
                        )
                    })?);
            }
            "-task-minutes" => {
                let number = value()?;
                let minutes: u64 = number.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    format!("`-task-minutes` needs a number above zero, not `{number}`")
                })?;
                parsed.task_seconds = Some(minutes.saturating_mul(60));
            }
            "-order" => {
                let name = value()?;
                parsed.order = Some(order(&name).ok_or(format!(
                    "unknown order `{name}`; use asked, reversed, finished or shuffled[:<seed>]"
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
                    None => {
                        parameter_options.push(param.split('=').next().unwrap_or(param).to_owned());
                        parsed.params.push((name, text));
                    }
                }
            }
            other if !other.starts_with('-') => match parsed.target {
                None => parsed.target = Some(PathBuf::from(other)),
                Some(_) => parsed.targets.push(other.to_owned()),
            },
            other => {
                return Err(engine_option(other, None)
                    .map(|help| format!("engine option `{other}` is unsupported; {help}"))
                    .unwrap_or_else(|| format!("unknown option `{other}`")));
            }
        }
    }
    let language = parsed.language.or_else(|| {
        parsed.target.as_deref().and_then(|target| {
            if matches!(
                target.file_name().and_then(|name| name.to_str()),
                Some("Snakefile" | "snakefile")
            ) || target
                .extension()
                .is_some_and(|extension| extension == "smk")
            {
                Some(Language::Snakemake)
            } else {
                crate::detect::language(target).ok()
            }
        })
    });
    for option in parameter_options {
        if let Some(help) = engine_option(&option, language) {
            return Err(format!("engine option `{option}` is unsupported; {help}"));
        }
        if language == Some(Language::Snakemake) {
            return Err(format!(
                "Snakemake option `{option}` is unsupported; use `--config name=value` for workflow parameters or `uwe run -help` for engine options"
            ));
        }
    }
    take_job_file(&mut parsed);
    Ok(parsed)
}

/// The words of a command line with the POSIX spellings `--name` and `--name=value` of the
/// options of uwe as `-name`. Commands with workflow parameters take them only before `--`,
/// after which `--name` stays a parameter, so that the command lines of before keep working.
fn posix(command: Option<&str>, words: &[String]) -> (Vec<String>, Option<usize>) {
    let Some(command) = command else {
        return (words.to_vec(), None);
    };
    let workflow = WORKFLOW.contains(&command);
    let delimiter = (workflow || command == "record")
        .then(|| words.iter().position(|word| word == "--"))
        .flatten();
    let end = match (workflow, delimiter) {
        (_, Some(at)) => at,
        (true, None) => 0,
        (false, None) => words.len(),
    };
    let mut spelled = Vec::with_capacity(words.len());
    for (at, word) in words.iter().enumerate() {
        let long = word
            .strip_prefix("--")
            .filter(|rest| at < end && !rest.is_empty());
        let (name, value) = match long.map(|rest| rest.split_once('=').unwrap_or((rest, ""))) {
            Some((name, value)) => (name, (word.contains('=')).then_some(value)),
            None => {
                spelled.push(word.clone());
                continue;
            }
        };
        let single = format!("-{name}");
        // `--config` stays Snakemake's config values, and `--configfile` takes its value.
        if name != "config" && known(command, &single) {
            spelled.push(single);
            spelled.extend(value.map(str::to_owned));
        } else {
            spelled.push(word.clone());
        }
    }
    let delimiter = delimiter.and_then(|_| spelled.iter().position(|word| word == "--"));
    (spelled, delimiter)
}

/// Whether `option` is an option of uwe that applies to `command`.
fn known(command: &str, option: &str) -> bool {
    OPTIONS.iter().any(|(names, commands)| {
        let flags = names.split_whitespace().next().unwrap_or_default();
        commands.contains(&command) && flags.split(',').any(|name| name == option)
    })
}

fn engine_option(option: &str, language: Option<Language>) -> Option<&'static str> {
    Some(match option {
        "-work-dir" | "-w" => "set `[cache] folder` in the uwe config file",
        "-with-docker" | "-with-podman" | "-with-singularity" | "-with-apptainer" => {
            "use `-executor local` and `-default-image image`, or `-executor slurm -slurm-container name`"
        }
        "-bg" => "run uwe in the foreground or use your service manager",
        "-with-report" => "use `-report file`",
        "-with-dag" => "use `-graph file`",
        "-with-trace" | "-with-timeline" => "use `-report file` or `uwe telemetry`",
        "-R" => "use `-forcerun rules,files` for a Snakemake workflow",
        "-F" => "use `-forceall` for a Snakemake workflow",
        "-j" | "-n" => {
            "use `-max-pending n` for concurrency or `-executor simulator` for a dry run"
        }
        _ if language == Some(Language::Snakemake) => match option {
            "--cores" | "--jobs" => {
                "use `-max-pending n` to limit concurrent tasks; task CPUs are defined by the workflow"
            }
            "--use-conda" => {
                "configure `[conda]` in the uwe config file; Conda preparation is container-only"
            }
            "--use-singularity" | "--use-apptainer" => {
                "use `-executor slurm -slurm-container apptainer`"
            }
            "--dry-run" => "use `-executor simulator`",
            "--profile" | "--workflow-profile" => {
                "use `-profile name` or `-profile path` for Snakemake profiles"
            }
            "--directory" => "launch uwe from the desired working directory",
            "--snakefile" => "give the Snakefile as the workflow argument",
            "--until" => "use `-until rules,files`",
            "--omit-from" => "use `-omit-from rules,files`",
            "--forcerun" => "use `-forcerun rules,files`",
            "--forceall" => "use `-forceall`",
            "--rerun-triggers" => {
                "use `-rerun-triggers mtime,params,input,software-env,code` or a subset"
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// Takes a job file after a CWL or WDL document as its inputs, as cwltool does in
/// `cwltool tool.cwl job.yml`. Other words after the target name a process of a packed
/// document.
fn take_job_file(parsed: &mut Arguments) {
    let extension = |name: &str| {
        (parsed.target.as_ref()).is_some_and(|target| target.extension() == Some(name.as_ref()))
    };
    let cwl = matches!(parsed.language, Some(Language::Cwl | Language::Wdl))
        || extension("cwl")
        || extension("wdl");
    let job = |word: &String| {
        [".json", ".yml", ".yaml"]
            .iter()
            .any(|end| word.ends_with(end))
    };
    if cwl
        && parsed.inputs.is_none()
        && let Some(index) = parsed.targets.iter().position(job)
    {
        parsed.inputs = Some(PathBuf::from(parsed.targets.remove(index)));
    }
}

/// Whether a word is a run id as uwe makes them: two hexadecimal numbers joined by `-`, the
/// time in milliseconds and the process id with random bits.
fn looks_like_run(text: &str) -> bool {
    text.split_once('-').is_some_and(|(time, process)| {
        let hex = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_hexdigit());
        hex(time) && hex(process)
    })
}

/// A percent like `10%` or `10` as a share from 0 to 1.
pub(super) fn percent(text: &str) -> Option<f64> {
    let number: f64 = text.strip_suffix('%').unwrap_or(text).parse().ok()?;
    (0.0..=100.0).contains(&number).then_some(number / 100.0)
}

/// A time like `90s`, `10m`, `2h` or `1d`; a plain number counts seconds.
pub(super) fn duration(text: &str) -> Option<std::time::Duration> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let seconds = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return None,
    };
    Some(std::time::Duration::from_secs(number.checked_mul(seconds)?))
}

/// A language by its name, as `-lang` takes it.
pub(crate) fn language(name: &str) -> Option<Language> {
    Some(match name.to_ascii_lowercase().as_str() {
        "nextflow" | "nf" => Language::Nextflow,
        "snakemake" | "smk" => Language::Snakemake,
        "cwl" => Language::Cwl,
        "argo" => Language::Argo,
        "wdl" => Language::Wdl,
        "wir" => Language::Wir,
        "galaxy" => Language::Galaxy,
        _ => return None,
    })
}

/// An order by its name, as `-order` takes it: `asked`, `reversed`, `finished`, `shuffled` or
/// `shuffled:<seed>`; the seed is 0 when not given.
pub(super) fn order(name: &str) -> Option<Order> {
    match name.split_once(':') {
        None if name == "asked" => Some(Order::AsAsked),
        None if name == "reversed" => Some(Order::Reversed),
        None if name == "finished" => Some(Order::Finished),
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
