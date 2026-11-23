//! Tests of reading command lines: spellings, values, groups and help.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::groups::GROUPS;
use super::options::COMMANDS;
use super::parse::{duration, order, percent};
use super::*;

fn parse_all(arguments: &[&str]) -> Arguments {
    let arguments: Vec<String> = arguments.iter().map(|text| text.to_string()).collect();
    parse(&arguments).expect("valid options")
}

#[test]
fn posix_spellings() {
    let words = |list: &[&str]| list.iter().map(|word| word.to_string()).collect::<Vec<_>>();
    // Without `--`, `--name` stays a workflow parameter, as before.
    let parsed = parse_command("run", &words(&["main.nf", "--profile", "test", "--help"]));
    let parsed = parsed.unwrap();
    assert_eq!(
        parsed.params,
        [
            ("profile".into(), "test".into()),
            ("help".into(), "true".into())
        ]
    );
    assert!(parsed.profiles.is_none() && !parsed.help);
    let line = [
        "main.nf",
        "--profile=test",
        "--no-cache",
        "--",
        "--profile",
        "x",
        "--version",
    ];
    let parsed = parse_command("run", &words(&line)).unwrap();
    assert_eq!(parsed.profiles, Some(vec!["test".to_owned()]));
    assert!(parsed.no_cache && !parsed.version);
    let given = [
        ("profile".into(), "x".into()),
        ("version".into(), "true".into()),
    ];
    assert_eq!(parsed.params, given);
    let parsed = parse_command("runs", &words(&["--json", "--database=x.sqlite"])).unwrap();
    assert!(parsed.json);
    assert_eq!(parsed.database, Some(PathBuf::from("x.sqlite")));
    let parsed = parse_command(
        "serve",
        &words(&["--port", "8080", "--tools", "a", "--tools=b"]),
    );
    let parsed = parsed.unwrap();
    assert_eq!((parsed.port, parsed.tools.len()), (Some(8080), 2));
    assert!(parse_command("runs", &words(&["--sample"])).is_err());
    let recorded = parse_command(
        "record",
        &words(&["--name", "copy", "--", "cp", "--name", "x"]),
    );
    let recorded = recorded.unwrap();
    assert_eq!(recorded.tool_name.as_deref(), Some("copy"));
    assert_eq!(recorded.command_line, ["cp", "--name", "x"]);
}

#[test]
fn ungroups_commands() {
    let words = |list: &[&str]| list.iter().map(|word| word.to_string()).collect::<Vec<_>>();
    let ungrouped = |command: &str, rest: &[&str]| ungroup(command, &words(rest));
    assert_eq!(ungrouped("runs", &[]).unwrap().0, "runs");
    assert_eq!(
        ungrouped("runs", &["-json"]).unwrap(),
        ("runs".into(), words(&["-json"]))
    );
    assert_eq!(
        ungrouped("runs", &["show", "a"]).unwrap(),
        ("runs show".into(), words(&["a"]))
    );
    assert_eq!(
        ungrouped("runs", &["compare", "a", "b"]).unwrap().0,
        "compare"
    );
    assert_eq!(ungrouped("tool", &[]).unwrap().0, "tool list");
    assert_eq!(ungrouped("tool", &["record", "--"]).unwrap().0, "record");
    assert_eq!(ungrouped("cache", &["digest", "x"]).unwrap().0, "digest");
    assert_eq!(ungrouped("doctor", &["-json"]).unwrap().0, "check-machine");
    assert_eq!(ungrouped("run", &["runs"]).unwrap().0, "run");
    assert!(ungrouped("cache", &[]).is_err() && ungrouped("runs", &["bogus"]).is_err());
    for command in typed_commands() {
        assert!(COMMANDS.contains(&command) || GROUPS.iter().any(|group| group.name == command));
    }
}

#[test]
fn record_takes_rest() {
    let words = ["-name", "copy", "--", "cat", "-n", "in.txt", ">", "out.txt"];
    let parsed = parse_command("record", &words.map(str::to_owned)).unwrap();
    assert_eq!(parsed.tool_name.as_deref(), Some("copy"));
    assert_eq!(parsed.command_line, ["cat", "-n", "in.txt", ">", "out.txt"]);
    assert!(parsed.params.is_empty() && parsed.target.is_none());
    assert!(parse_command("run", &["main.nf", "-name", "x"].map(str::to_owned)).is_err());
}

#[test]
fn repository_options_scoped() {
    let words = ["owner/repo", "-r", "feature/new", "-latest"].map(str::to_owned);
    let parsed = parse_command("run", &words).unwrap();
    assert_eq!(parsed.revision.as_deref(), Some("feature/new"));
    assert!(parsed.latest);
    assert!(parse_command("repeat", &words).is_err());
    assert!(
        parse_all(&["owner/repo", "-revision", "v1"])
            .revision
            .is_some()
    );
    for words in [["-r", "-latest"], ["-r", ""]] {
        assert!(parse(&words.map(str::to_owned)).is_err());
    }
}

#[test]
fn parses_workflow_presets() {
    let words = ["-preset", "lab"].map(str::to_owned);
    for command in ["run", "check", "inputs", "outputs"] {
        assert_eq!(
            parse_command(command, &words).unwrap().preset.as_deref(),
            Some("lab")
        );
    }
    assert!(parse_command("runs", &words).is_err());
}

#[test]
fn scopes_safety_options() {
    let parsed = parse_command("run", &["main.nf".into(), "-strict-provenance".into()]).unwrap();
    assert!(parsed.run_settings().strict_provenance);
    assert!(!Arguments::default().run_settings().strict_provenance);
    assert!(parse_command("graph", &["-strict-provenance".into()]).is_err());
    assert!(help("run").unwrap().contains("-strict-provenance"));
    assert!(parse_command("clean", &["-front".into()]).unwrap().front);
    assert!(parse_command("run", &["-front".into()]).is_err());
    assert!(help("clean").unwrap().contains("-front"));
    let words = [
        "run-id",
        "-reason",
        "owner process exited",
        "-database",
        "runs.sqlite",
    ]
    .map(str::to_owned);
    assert_eq!(
        parse_command("reconcile", &words)
            .unwrap()
            .reason
            .as_deref(),
        Some("owner process exited")
    );
    assert!(parse_command("run", &words).is_err());
    for reason in ["", "  ", "-database"] {
        assert!(parse_command("reconcile", &["-reason".into(), reason.into()]).is_err());
    }
    assert!(help("reconcile").unwrap().contains("-reason <text>"));
    assert!(help("digest").unwrap().contains("<path>"));
}

#[test]
fn retry_numbers_bounded() {
    let parsed = parse_all(&["-machine-retries", "0", "-machine-retry-delay-ms", "250"]);
    assert_eq!(parsed.machine_retries, Some(0));
    assert_eq!(parsed.retry_delay_ms, Some(250));
    assert_eq!(parse_all(&["-task-minutes", "90"]).task_seconds, Some(5400));
    for pair in [
        ["-machine-retries", "-1"],
        ["-machine-retry-delay-ms", "0"],
        ["-task-minutes", "0"],
    ] {
        assert!(parse(&pair.map(str::to_owned)).is_err());
    }
}

#[test]
fn target_options_scoped() {
    let words = [
        "Snakefile",
        "-until",
        "a,b.txt",
        "-until",
        "c",
        "-omit-from",
        "d",
        "-forcerun",
        "e,f.txt",
        "-forceall",
    ]
    .map(str::to_owned);
    for command in ["run", "graph", "check", "check-order"] {
        let parsed = parse_command(command, &words).unwrap();
        assert_eq!(parsed.until, ["a", "b.txt", "c"]);
        assert_eq!(parsed.omit_from, ["d"]);
        assert_eq!(parsed.forcerun, ["e", "f.txt"]);
        assert!(parsed.forceall);
        assert!(parsed.targets.is_empty());
    }
    for command in ["inputs", "outputs", "config", "repeat", "runs"] {
        assert!(
            parse_command(command, &words)
                .unwrap_err()
                .contains("does not apply")
        );
    }
    for option in ["-until", "-omit-from", "-forcerun"] {
        for value in [None, Some("-forceall"), Some("a,"), Some("")] {
            let mut words = vec!["Snakefile".into(), option.into()];
            words.extend(value.map(str::to_owned));
            assert!(parse(&words).unwrap_err().contains("needs"));
        }
    }
    for (option, replacement) in [
        ("--until", "-until"),
        ("--omit-from", "-omit-from"),
        ("--forcerun", "-forcerun"),
        ("-R", "-forcerun"),
        ("--forceall", "-forceall"),
        ("-F", "-forceall"),
    ] {
        let error = parse(&["Snakefile".into(), option.into()]).unwrap_err();
        assert!(error.contains(&format!("use `{replacement}")), "{error}");
    }
}

#[test]
fn collects_uncached_steps() {
    let words = [
        "main.nf",
        "-no-cache-steps",
        "FASTQC,align/sort",
        "-no-cache-steps",
        "MULTIQC",
    ]
    .map(str::to_owned);
    for command in ["run", "graph", "check-order"] {
        let parsed = parse_command(command, &words).unwrap();
        assert_eq!(parsed.no_cache_steps, ["FASTQC", "align/sort", "MULTIQC"]);
        let settings = parsed.run_settings();
        assert_eq!(settings.no_cache_steps, parsed.no_cache_steps);
    }
    let error = parse_command("repeat", &words).unwrap_err();
    assert!(error.contains("does not apply"), "{error}");
    for value in [None, Some("-no-cache"), Some("a,"), Some("")] {
        let mut words = vec!["main.nf".into(), "-no-cache-steps".into()];
        words.extend(value.map(str::to_owned));
        assert!(parse(&words).unwrap_err().contains("needs"));
    }
}

#[test]
fn engine_flags_reserved() {
    for flag in [
        "--cores",
        "--use-conda",
        "--use-singularity",
        "--dry-run",
        "--profile",
        "--forceall",
        "-j",
        "-n",
    ] {
        let words = ["Snakefile".to_owned(), flag.to_owned(), "8".to_owned()];
        assert!(
            parse(&words).unwrap_err().contains("unsupported;"),
            "{flag}"
        );
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
    assert_eq!(
        parse_all(&["Snakefile", "--config=cores=8"]).params,
        [("cores".into(), "8".into())]
    );
    assert_eq!(
        parse_all(&["Snakefile", "-profile", "lab"]).profiles,
        Some(vec!["lab".into()])
    );
    assert_eq!(
        parse_all(&["main.nf", "--help"]).params,
        [("help".into(), "true".into())]
    );
}

#[test]
fn clean_rules_parsed() {
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
fn network_switch_parsed() {
    assert!(parse_all(&["main.nf", "-no-network"]).no_network);
    assert!(!parse_all(&["main.nf"]).no_network);
}

#[test]
fn check_percent_parsed() {
    let parsed = parse_all(&["-check-reproducibility", "10%"]);
    assert_eq!(parsed.check_reproducibility, Some(0.1));
    assert_eq!(percent("100"), Some(1.0));
    assert_eq!(percent("0.5%"), Some(0.005));
    assert_eq!(percent("120%"), None);
    assert_eq!(percent("some"), None);
}

#[test]
fn parameter_values_parsed() {
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
fn parses_rerun_triggers() {
    let parsed = parse_all(&["Snakefile", "-rerun-triggers", "mtime,code"]);
    assert_eq!(
        parsed.rerun_triggers,
        Some(vec!["mtime".into(), "code".into()])
    );
    for value in ["", "unknown", "mtime,"] {
        assert!(parse(&["Snakefile".into(), "-rerun-triggers".into(), value.into()]).is_err());
    }
    assert!(help("run").unwrap().contains("-rerun-triggers <a,b>"));
    assert!(!help("graph").unwrap().contains("-rerun-triggers <a,b>"));
}

#[test]
fn graph_options_parsed() {
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
fn config_parameter_parsed() {
    let parsed = parse_all(&["main.nf", "--config", "file.txt"]);
    assert_eq!(
        parsed.params,
        [("config".to_owned(), "file.txt".to_owned())]
    );
}

#[test]
fn store_locations_parsed() {
    let parsed = parse_all(&["main.nf", "-store", "s3://cache/uwe"]);
    assert_eq!(parsed.store.as_deref(), Some("s3://cache/uwe"));
    let parsed = parse_all(&["main.nf", "-store", "cache", "-uwe-config", "lab.toml"]);
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
fn scheduling_options_parsed() {
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
fn resume_id_optional() {
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
fn cwl_inputs_positional() {
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
fn cache_optional() {
    assert!(!parse_all(&["main.nf"]).no_cache);
    assert!(parse_all(&["main.nf", "-no-cache"]).no_cache);
    assert!(parse_all(&["main.nf", "-no-right-sizing"]).no_right_sizing);
    assert!(parse_all(&["main.nf", "-no-grouping"]).no_grouping);
}

#[test]
fn rejects_unknown_options() {
    let arguments = ["-lang".to_owned(), "taverna".to_owned()];
    assert!(parse(&arguments).is_err());
    assert!(parse(&["-x".to_owned()]).is_err());
}

#[test]
fn delimiter_takes_parameters() {
    let words = |list: &[&str]| list.iter().map(|word| word.to_string()).collect::<Vec<_>>();
    let line = [
        "Snakefile",
        "--",
        "--config",
        "a=1",
        "--rerun-incomplete",
        "--threads=4",
        "--configfile",
        "x.yaml",
    ];
    let parsed = parse_command("run", &words(&line)).expect("parameters after `--`");
    let given = [
        ("config".into(), "a=1".into()),
        ("rerun-incomplete".into(), "true".into()),
        ("threads".into(), "4".into()),
        ("configfile".into(), "x.yaml".into()),
    ];
    assert_eq!(parsed.params, given);
    assert!(parsed.configs.is_empty());
    let refused = parse_command("run", &words(&["main.nf", "--", "-profile", "x"]));
    assert!(refused.is_err());
}

#[test]
fn debug_hides_secrets() {
    let arguments = Arguments {
        target: Some("main.nf".into()),
        token: Some("page-token-value".into()),
        params: vec![("password".into(), "parameter-secret".into())],
        ..Arguments::default()
    };
    let shown = format!("{arguments:?}");
    assert!(
        shown.contains("main.nf") && shown.contains("password"),
        "{shown}"
    );
    assert!(
        !shown.contains("page-token-value") && !shown.contains("parameter-secret"),
        "{shown}"
    );
}
