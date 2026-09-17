//! The command line of `iunctus run`, `iunctus graph` and `iunctus config`: Nextflow's options,
//! and those that the other languages add.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::net::IpAddr;
use std::path::PathBuf;

use workflow_graph::render::{Direction, Format, View};
use workflow_graph::{Language, Order};

/// The options of a command.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arguments {
    /// The workflow file or project directory.
    pub target: Option<PathBuf>,
    /// `-lang name`; else the language is found from the target.
    pub language: Option<Language>,
    pub stub: bool,
    /// `-keep-going`: after a task failed, run every task that does not depend on it.
    pub keep_going: bool,
    /// `-store folder|address`: where the cache store lives; a folder is made absolute.
    pub store: Option<String>,
    /// `-iunctus-config file`: a config file of iunctus, read after the site-wide file and
    /// the user's file.
    pub config_file: Option<PathBuf>,
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
    /// `-executor simulator|local|tes|slurm|kubernetes`; `None` for the default.
    pub executor: Option<String>,
    /// `-graph file`: where to write the execution graph.
    pub graph: Option<PathBuf>,
    /// `-report file`: where `iunctus run` writes the report of the run when it ends.
    pub report: Option<PathBuf>,
    /// `-output-dir folder`: where results go that the workflow gives no place of its own.
    pub output_dir: Option<PathBuf>,
    /// `-tes-server address`: the TES server of `-executor tes`.
    pub tes_server: Option<String>,
    /// `-slurm-container apptainer|singularity|podman|docker`: how the nodes of
    /// `-executor slurm` run containers.
    pub slurm_container: Option<String>,
    /// `-slurm-options 'options'`: more options of `sbatch`, like `--account=lab`.
    pub slurm_options: Option<String>,
    /// `-kubeconfig file`: the kubeconfig of the cluster of `-executor kubernetes`.
    pub kubeconfig: Option<PathBuf>,
    /// `-kubernetes-namespace name`: the namespace of the Jobs of `-executor kubernetes`.
    pub kubernetes_namespace: Option<String>,
    /// `-format name`: `json`, `dot`, `summary` or `svg` for `iunctus graph`; `json`, `text` or
    /// `form` for `iunctus inputs`.
    pub format: Option<String>,
    /// `-check`: `iunctus inputs` also checks the values of the launch against the schema.
    pub check: bool,
    /// `-view tasks|steps`: the graph with one node per task, or one node per step.
    pub view: Option<View>,
    /// `-direction down|right`: which way a picture of the graph goes.
    pub direction: Option<Direction>,
    /// `-fold n`: in a picture of the graph of tasks, a step with more than n tasks is one
    /// box; 0 draws every task.
    pub fold: Option<usize>,
    /// `-max-pending n`: how many tasks may be pending at once.
    pub max_pending: Option<usize>,
    /// `-order asked|reversed|finished|shuffled[:seed]`: the order in which pending tasks
    /// finish.
    pub order: Option<Order>,
    /// `-default-image image`: the container image of tasks that name none, for real executors.
    pub default_image: Option<String>,
    /// `-python-image image`: the image of the Python container of decision 19, with the
    /// Python packages that the workflow imports.
    pub python_image: Option<String>,
    /// `-database file`: the run database, instead of the one in the cache directory.
    pub database: Option<PathBuf>,
    /// `-output folder|file`: where `iunctus export` writes the crate, `iunctus report` the
    /// report, and some other commands their file.
    pub output: Option<PathBuf>,
    /// `-platform name`: the conda platform of a lock, like `linux-64`.
    pub platform: Option<String>,
    /// `-license url`: the license of an exported crate.
    pub license: Option<String>,
    /// `-resume [run]`: continue a run, by default the last run of the workflow.
    pub resume: Option<Resume>,
    /// `-page`: serve the web page of the run while it goes on.
    pub page: bool,
    /// `-address ip`: where the web page listens; only this machine by default.
    pub address: Option<IpAddr>,
    /// `-port number`: the port of the web page; a free one by default.
    pub port: Option<u16>,
    /// `-keep-days n`: `iunctus clean` keeps what runs made or used in the last n days.
    pub keep_days: Option<u64>,
    /// `-keep-run run`, as often as needed: `iunctus clean` keeps what the run needs.
    pub keep_runs: Vec<String>,
    /// `-keep-slower-than time`, like `10m`: `iunctus clean` keeps the results of tasks that
    /// ran at least this long.
    pub keep_slower_than: Option<std::time::Duration>,
    /// `-remove`: `iunctus clean` removes what it lists, instead of only listing it.
    pub remove: bool,
    /// `-check-reproducibility percent`, like `10%`: the share of the tasks whose results come
    /// from the cache that run again, to check that their tools give the same bytes; from 0 to 1.
    pub check_reproducibility: Option<f64>,
    /// `-no-cache`: the run takes no result from the cache and keeps none there; every task
    /// runs.
    pub no_cache: bool,
    /// `-no-network`: tasks whose workflow does not say that they need the network run
    /// without it.
    pub no_network: bool,
    /// `-no-right-sizing`: tasks ask for the CPUs and memory that their workflow names, not
    /// for what the tasks of their step used in earlier runs.
    pub no_right_sizing: bool,
    /// `-no-grouping`: each task runs in a container of its own, also the short tasks that
    /// would run together.
    pub no_grouping: bool,
}

/// Which run `-resume` continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    /// The last run of the same workflow.
    Last,
    /// The run with this id.
    Run(String),
}

/// Reads the options after the command name.
pub fn parse(arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut parameter_options = Vec::new();
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
            "-keep-going" => parsed.keep_going = true,
            "-store" => {
                let place = value()?;
                parsed.store = Some(match storage::address::scheme(&place) {
                    Some(_) => place,
                    None => std::path::absolute(&place)
                        .map_or(place, |folder| folder.to_string_lossy().into_owned()),
                });
            }
            "-iunctus-config" => parsed.config_file = Some(value()?.into()),
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
            "-report" => parsed.report = Some(value()?.into()),
            "-output-dir" => parsed.output_dir = Some(value()?.into()),
            "-tes-server" => parsed.tes_server = Some(value()?),
            "-slurm-container" => parsed.slurm_container = Some(value()?),
            "-slurm-options" => parsed.slurm_options = Some(value()?),
            "-kubeconfig" => parsed.kubeconfig = Some(value()?.into()),
            "-kubernetes-namespace" => parsed.kubernetes_namespace = Some(value()?),
            "-format" => {
                let name = value()?;
                if Format::parse(&name).is_none() && !matches!(name.as_str(), "text" | "form") {
                    return Err(format!(
                        "unknown graph format `{name}`; use json, dot, summary or svg, or json, \
                         text or form for `iunctus inputs`"
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
            "-remove" => parsed.remove = true,
            "-no-network" => parsed.no_network = true,
            "-check-reproducibility" => {
                let text = value()?;
                parsed.check_reproducibility = Some(percent(&text).ok_or(format!(
                    "`-check-reproducibility` needs a share of the cached tasks from 0% to 100%, \
                     like 10%, not `{text}`"
                ))?);
            }
            "-no-cache" => parsed.no_cache = true,
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
                parameter_options.push(param.split('=').next().unwrap_or(param).to_owned());
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
    }
    if language == Some(Language::Snakemake) && parsed.profiles.is_some() {
        return Err(
            "Snakemake profiles are unsupported; use `-iunctus-config file` for executor settings"
                .into(),
        );
    }
    take_job_file(&mut parsed);
    Ok(parsed)
}

fn engine_option(option: &str, language: Option<Language>) -> Option<&'static str> {
    Some(match option {
        "-work-dir" | "-w" => "set `[cache] folder` in the iunctus config file",
        "-with-docker" | "-with-podman" | "-with-singularity" | "-with-apptainer" => {
            "use `-executor local` and `-default-image image`, or `-executor slurm -slurm-container name`"
        }
        "-bg" => "run iunctus in the foreground or use your service manager",
        "-with-report" => "use `-report file`",
        "-with-dag" => "use `-graph file`",
        "-with-trace" | "-with-timeline" => "use `-report file` or `iunctus telemetry`",
        "-j" | "-n" => {
            "use `-max-pending n` for concurrency or `-executor simulator` for a dry run"
        }
        _ if language == Some(Language::Snakemake) => match option {
            "--cores" | "--jobs" => {
                "use `-max-pending n` for concurrency and `[executor.local] cpus` for the CPU pool"
            }
            "--use-conda" => {
                "configure `[conda]` in the iunctus config file; Conda preparation is container-only"
            }
            "--use-singularity" | "--use-apptainer" => {
                "use `-executor slurm -slurm-container apptainer`"
            }
            "--dry-run" => "use `-executor simulator`",
            "--profile" | "--workflow-profile" => {
                "use `-iunctus-config file`; Snakemake profiles are unsupported"
            }
            "--directory" => "launch iunctus from the desired working directory",
            "--snakefile" => "give the Snakefile as the workflow argument",
            _ => return None,
        },
        _ => return None,
    })
}

/// Takes a job file after a CWL document as its inputs, as cwltool does in
/// `cwltool tool.cwl job.yml`. Other words after the target name a process of a packed
/// document.
fn take_job_file(parsed: &mut Arguments) {
    let cwl = parsed.language == Some(Language::Cwl)
        || (parsed.target.as_ref())
            .is_some_and(|target| target.extension() == Some("cwl".as_ref()));
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

/// Whether a word is a run id as iunctus makes them: two hexadecimal numbers joined by `-`, the
/// time in milliseconds and the process id.
fn looks_like_run(text: &str) -> bool {
    text.split_once('-').is_some_and(|(time, process)| {
        let hex = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_hexdigit());
        hex(time) && hex(process)
    })
}

/// A percent like `10%` or `10` as a share from 0 to 1.
fn percent(text: &str) -> Option<f64> {
    let number: f64 = text.strip_suffix('%').unwrap_or(text).parse().ok()?;
    (0.0..=100.0).contains(&number).then_some(number / 100.0)
}

/// A time like `90s`, `10m`, `2h` or `1d`; a plain number counts seconds.
fn duration(text: &str) -> Option<std::time::Duration> {
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
fn language(name: &str) -> Option<Language> {
    Some(match name.to_ascii_lowercase().as_str() {
        "nextflow" | "nf" => Language::Nextflow,
        "snakemake" | "smk" => Language::Snakemake,
        "cwl" => Language::Cwl,
        "argo" => Language::Argo,
        _ => return None,
    })
}

/// An order by its name, as `-order` takes it: `asked`, `reversed`, `finished`, `shuffled` or
/// `shuffled:<seed>`; the seed is 0 when not given.
fn order(name: &str) -> Option<Order> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(arguments: &[&str]) -> Arguments {
        let arguments: Vec<String> = arguments.iter().map(|text| text.to_string()).collect();
        parse(&arguments).expect("valid options")
    }

    #[test]
    fn engine_flags_are_not_workflow_parameters() {
        for flag in [
            "--cores",
            "--use-conda",
            "--use-singularity",
            "--dry-run",
            "--profile",
            "-j",
            "-n",
        ] {
            let words = ["Snakefile".to_owned(), flag.to_owned(), "8".to_owned()];
            assert!(parse(&words).unwrap_err().contains("unsupported;"), "{flag}");
        }
        for flag in ["-work-dir", "-w", "-with-docker", "-bg", "-with-report"] {
            let words = ["main.nf".to_owned(), flag.to_owned()];
            assert!(parse(&words).unwrap_err().contains("unsupported"), "{flag}");
        }
        assert_eq!(
            parse_all(&["main.nf", "--cores", "8"]).params,
            [("cores".into(), "8".into())]
        );
        assert_eq!(
            parse_all(&["Snakefile", "--config", "cores=8"]).params,
            [("cores".into(), "8".into())]
        );
    }

    #[test]
    fn clean_up_takes_its_rules() {
        let parsed = parse_all(&[
            "-keep-days",
            "30",
            "-keep-run",
            "1a-2b",
            "-keep-run",
            "3c-4d",
            "-keep-slower-than",
            "10m",
            "-remove",
        ]);
        assert_eq!(parsed.keep_days, Some(30));
        assert_eq!(parsed.keep_runs, ["1a-2b", "3c-4d"]);
        assert_eq!(
            parsed.keep_slower_than,
            Some(std::time::Duration::from_secs(600))
        );
        assert!(parsed.remove);
        assert_eq!(duration("90"), Some(std::time::Duration::from_secs(90)));
        assert_eq!(duration("1d"), Some(std::time::Duration::from_secs(86_400)));
        assert_eq!(duration("1.5h"), None);
        assert_eq!(duration("m"), None);
    }

    #[test]
    fn no_network_is_a_switch() {
        assert!(parse_all(&["main.nf", "-no-network"]).no_network);
        assert!(!parse_all(&["main.nf"]).no_network);
    }

    #[test]
    fn the_reproducibility_check_takes_a_percent() {
        let parsed = parse_all(&["-check-reproducibility", "10%"]);
        assert_eq!(parsed.check_reproducibility, Some(0.1));
        assert_eq!(percent("100"), Some(1.0));
        assert_eq!(percent("0.5%"), Some(0.005));
        assert_eq!(percent("120%"), None);
        assert_eq!(percent("some"), None);
    }

    #[test]
    fn params_take_their_values() {
        let parsed = parse_all(&[
            "main.nf",
            "--outdir",
            "out",
            "--skip",
            "--n",
            "-1",
            "-stub",
            "-keep-going",
        ]);
        assert_eq!(parsed.target, Some(PathBuf::from("main.nf")));
        let params = [("outdir", "out"), ("skip", "true"), ("n", "-1")];
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()))
            .collect();
        assert_eq!(parsed.params, params);
        assert!(parsed.stub);
        assert!(parsed.keep_going);
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
    fn pictures_take_a_direction_and_a_fold() {
        let parsed = parse_all(&["main.nf", "-direction", "right", "-fold", "0"]);
        assert_eq!(parsed.direction, Some(Direction::Right));
        assert_eq!(parsed.fold, Some(0));
        let arguments = [
            "main.nf".to_owned(),
            "-direction".to_owned(),
            "up".to_owned(),
        ];
        let error = parse(&arguments).expect_err("an unknown direction");
        assert!(error.contains("use down or right"), "{error}");
        let arguments = ["main.nf".to_owned(), "-fold".to_owned(), "many".to_owned()];
        assert!(parse(&arguments).is_err());
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
    fn stores_are_folders_or_addresses() {
        let parsed = parse_all(&["main.nf", "-store", "s3://cache/iunctus"]);
        assert_eq!(parsed.store.as_deref(), Some("s3://cache/iunctus"));
        let parsed = parse_all(&["main.nf", "-store", "cache", "-iunctus-config", "lab.toml"]);
        let folder = std::path::absolute("cache").expect("an absolute path");
        assert_eq!(parsed.store, Some(folder.to_string_lossy().into_owned()));
        assert_eq!(parsed.config_file, Some(PathBuf::from("lab.toml")));
    }

    #[test]
    fn slurm_options() {
        let parsed = parse_all(&[
            "main.nf",
            "-executor",
            "slurm",
            "-slurm-container",
            "podman",
            "-slurm-options",
            "--account=lab --qos=low",
        ]);
        assert_eq!(parsed.slurm_container.as_deref(), Some("podman"));
        assert_eq!(
            parsed.slurm_options.as_deref(),
            Some("--account=lab --qos=low")
        );
    }

    #[test]
    fn kubernetes_options() {
        let arguments = [
            "main.nf",
            "-kubeconfig",
            "kind.yaml",
            "-kubernetes-namespace",
            "x",
        ];
        let parsed = parse_all(&arguments);
        assert_eq!(parsed.kubeconfig, Some(PathBuf::from("kind.yaml")));
        assert_eq!(parsed.kubernetes_namespace.as_deref(), Some("x"));
    }

    #[test]
    fn orders_and_pending_limits() {
        let parsed = parse_all(&["main.nf", "-max-pending", "4", "-order", "shuffled:7"]);
        assert_eq!(parsed.default_image, None);
        let parsed = parse_all(&["tool.cwl", "-output-dir", "out"]);
        assert_eq!(parsed.output_dir, Some(PathBuf::from("out")));
        let parsed = parse_all(&["Snakefile", "-python-image", "python:3.12"]);
        assert_eq!(parsed.python_image.as_deref(), Some("python:3.12"));
        let parsed = parse_all(&["main.nf", "-max-pending", "4", "-order", "shuffled:7"]);
        assert_eq!(parsed.max_pending, Some(4));
        assert_eq!(parsed.order, Some(Order::Shuffled { seed: 7 }));
        assert_eq!(order("reversed"), Some(Order::Reversed));
        assert_eq!(order("finished"), Some(Order::Finished));
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
    fn run_database_options() {
        let parsed = parse_all(&["19a2b-3c", "-database", "runs.sqlite", "-output", "crate"]);
        assert_eq!(parsed.target, Some(PathBuf::from("19a2b-3c")));
        assert_eq!(parsed.database, Some(PathBuf::from("runs.sqlite")));
        assert_eq!(parsed.output, Some(PathBuf::from("crate")));
        let parsed = parse_all(&["19a2b-3c", "-license", "https://spdx.org/licenses/MIT"]);
        assert_eq!(
            parsed.license.as_deref(),
            Some("https://spdx.org/licenses/MIT")
        );
    }

    #[test]
    fn resume_takes_a_run_when_one_follows() {
        let parsed = parse_all(&["main.nf", "-resume"]);
        assert_eq!(parsed.resume, Some(Resume::Last));
        let parsed = parse_all(&["main.nf", "-resume", "19a8f3c2b10-1a2b", "-page"]);
        assert_eq!(parsed.resume, Some(Resume::Run("19a8f3c2b10-1a2b".into())));
        assert!(parsed.page);
        let parsed = parse_all(&["-resume", "main.nf"]);
        assert_eq!(parsed.resume, Some(Resume::Last));
        assert_eq!(parsed.target, Some(PathBuf::from("main.nf")));
        let parsed = parse_all(&["-address", "0.0.0.0", "-port", "8080"]);
        assert_eq!(parsed.address, Some("0.0.0.0".parse().expect("an address")));
        assert_eq!(parsed.port, Some(8080));
        assert!(parse(&["-port".to_owned(), "web".to_owned()]).is_err());
    }

    #[test]
    fn a_job_file_after_a_cwl_document_is_its_inputs() {
        let parsed = parse_all(&["tool.cwl", "job.yml"]);
        assert_eq!(parsed.inputs, Some(PathBuf::from("job.yml")));
        assert!(parsed.targets.is_empty());
        let parsed = parse_all(&["packed.cwl", "main", "job.json"]);
        assert_eq!(parsed.inputs, Some(PathBuf::from("job.json")));
        assert_eq!(parsed.targets, ["main"]);
        let parsed = parse_all(&["tool.cwl", "-inputs", "a.yml", "b.yml"]);
        assert_eq!(parsed.inputs, Some(PathBuf::from("a.yml")));
        assert_eq!(parsed.targets, ["b.yml"]);
        // Argo and Snakemake keep their targets.
        let parsed = parse_all(&["workflow.yaml", "values.yaml"]);
        assert_eq!(
            (parsed.inputs, parsed.targets),
            (None, vec!["values.yaml".to_owned()])
        );
    }

    #[test]
    fn runs_can_go_without_the_cache() {
        assert!(!parse_all(&["main.nf"]).no_cache);
        assert!(parse_all(&["main.nf", "-no-cache"]).no_cache);
        assert!(parse_all(&["main.nf", "-no-right-sizing"]).no_right_sizing);
        assert!(parse_all(&["main.nf", "-no-grouping"]).no_grouping);
    }

    #[test]
    fn unknown_languages_and_options_are_errors() {
        let arguments = ["-lang".to_owned(), "wdl".to_owned()];
        assert!(parse(&arguments).is_err());
        assert!(parse(&["-x".to_owned()]).is_err());
    }
}
