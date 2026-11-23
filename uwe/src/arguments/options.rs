//! The commands of uwe and the options that each one takes, with the help of each option.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub const COMMANDS: &[&str] = &[
    "run",
    "graph",
    "check",
    "check-machine",
    "config",
    "runs",
    "export",
    "page",
    "why",
    "origin",
    "digest",
    "inputs",
    "outputs",
    "clean",
    "check-order",
    "changes",
    "repeat",
    "lock-environment",
    "time",
    "compare",
    "replay",
    "telemetry",
    "report",
    "resume",
    "reconcile",
    "presets",
    "kuberun",
    "record",
    "lower",
    "serve",
    "init",
    "completions",
    "runs show",
    "runs logs",
    "runs cancel",
    "tool list",
    "tool inspect",
    "tool extract",
    "tool check",
    "tool suggest",
    "version",
];

/// The options of a command, each as `-name`.
pub(crate) fn options_of(command: &str) -> Vec<&'static str> {
    let applying = OPTIONS
        .iter()
        .filter(|(_, commands)| commands.contains(&command));
    let flags = applying.flat_map(|(names, _)| {
        names
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .split(',')
    });
    flags.filter(|flag| !flag.starts_with("--")).collect()
}

pub(super) const WORKFLOW: &[&str] = &[
    "run",
    "graph",
    "lower",
    "check",
    "config",
    "inputs",
    "outputs",
    "check-order",
    "kuberun",
];
pub(super) const CONFIGURED: &[&str] = COMMANDS.split_at(COMMANDS.len() - 1).0;
pub(super) const EXECUTION: &[&str] = &["run", "graph", "check-order", "repeat", "replay"];
pub(super) const BACKEND: &[&str] = &[
    "run",
    "check",
    "graph",
    "check-order",
    "repeat",
    "replay",
    "check-machine",
    "runs cancel",
];
/// The commands of `uwe tool` that read the tool library.
pub(super) const LIBRARY: &[&str] = &[
    "tool list",
    "tool inspect",
    "tool check",
    "tool suggest",
    "serve",
    "page",
];
/// The commands that can write JSON instead of text.
pub(super) const JSON: &[&str] = &[
    "runs",
    "runs show",
    "tool list",
    "tool inspect",
    "tool extract",
    "tool check",
    "tool suggest",
    "check-machine",
    "digest",
];
pub(super) const KUBERNETES: &[&str] = &[
    "run",
    "check",
    "graph",
    "check-order",
    "repeat",
    "replay",
    "check-machine",
    "kuberun",
];
pub(super) const OPTIONS: &[(&str, &[&str])] = &[
    (
        "-r,-revision <tag|branch|commit>",
        &["run", "graph", "check", "inputs", "outputs", "kuberun"],
    ),
    ("-latest", &["run", "graph", "kuberun"]),
    ("-name <name>", &["record", "init"]),
    ("-json", JSON),
    ("-tools <folder>", LIBRARY),
    ("-workflows <folder>", &["serve", "page"]),
    ("-token <token>", &["serve", "page"]),
    ("-open", &["serve", "page"]),
    ("-image <image>", &["record"]),
    ("-help", COMMANDS),
    ("-version,--version", COMMANDS),
    ("-v,-verbose", CONFIGURED),
    ("-quiet", CONFIGURED),
    ("-uwe-config <file>", CONFIGURED),
    (
        "-database <file>",
        &[
            "run",
            "graph",
            "check-machine",
            "runs",
            "runs show",
            "runs logs",
            "runs cancel",
            "serve",
            "export",
            "page",
            "why",
            "origin",
            "clean",
            "check-order",
            "changes",
            "repeat",
            "time",
            "compare",
            "replay",
            "telemetry",
            "report",
            "reconcile",
        ],
    ),
    (
        "-lang <nextflow|snakemake|cwl|argo|wdl|galaxy>",
        &[
            "run",
            "graph",
            "check",
            "inputs",
            "outputs",
            "check-order",
            "kuberun",
        ],
    ),
    ("-profile <a,b>", WORKFLOW),
    ("-preset <name>", &["run", "check", "inputs", "outputs"]),
    ("-c,-config,-configfile,--configfile <file>", WORKFLOW),
    ("-inputs,-params-file <file>", WORKFLOW),
    (
        "-entry <name>",
        &[
            "run",
            "graph",
            "check",
            "inputs",
            "outputs",
            "check-order",
            "kuberun",
        ],
    ),
    ("--config <name=value>", WORKFLOW),
    (
        "-stub,-stub-run",
        &["run", "graph", "check", "check-order", "kuberun"],
    ),
    ("-keep-going", &["run", "graph", "check-order"]),
    (
        "-until <rules,files>",
        &["run", "graph", "check", "check-order"],
    ),
    (
        "-omit-from <rules,files>",
        &["run", "graph", "check", "check-order"],
    ),
    (
        "-forcerun <rules,files>",
        &["run", "graph", "check", "check-order"],
    ),
    ("-forceall", &["run", "graph", "check", "check-order"]),
    ("-rerun-triggers <a,b>", &["run"]),
    (
        "-executor <simulator|local|tes|slurm|kubernetes|aws-batch|google-batch|azure-batch|nomad|htcondor|pbs|flyte|snowflake|jarvice|wr>",
        BACKEND,
    ),
    (
        "-store <folder|address>",
        &[
            "run",
            "graph",
            "check-machine",
            "export",
            "clean",
            "check-order",
            "repeat",
            "replay",
            "kuberun",
            "serve",
            "runs cancel",
        ],
    ),
    ("-tes-server <address>", BACKEND),
    (
        "-slurm-container <apptainer|singularity|podman|docker>",
        BACKEND,
    ),
    ("-slurm-options <options>", BACKEND),
    ("-kubeconfig <file>", KUBERNETES),
    ("-kubernetes-namespace <name>", KUBERNETES),
    ("-head-image <image>", &["kuberun"]),
    ("-head-cpus <n>", &["kuberun"]),
    ("-head-memory <size>", &["kuberun"]),
    ("-volume-mount <claim:path,...>", &["kuberun"]),
    ("-background", &["kuberun"]),
    (
        "-default-image <image>",
        &[
            "run",
            "graph",
            "check",
            "check-order",
            "repeat",
            "replay",
            "kuberun",
        ],
    ),
    (
        "-python-image <image>",
        &["run", "graph", "check", "inputs", "outputs", "check-order"],
    ),
    ("-output-dir <folder>", WORKFLOW),
    ("-workdir <folder>", WORKFLOW),
    ("-graph <file>", &["run", "graph"]),
    ("-report <file>", &["run", "compare"]),
    ("-fail-on-change", &["compare"]),
    (
        "-format <format>",
        &["graph", "inputs", "outputs", "time", "compare", "record"],
    ),
    ("-check", &["inputs"]),
    ("-view <tasks|steps>", &["run", "graph"]),
    ("-direction <down|right>", &["run", "graph"]),
    ("-fold <n>", &["run", "graph"]),
    (
        "-max-pending <n>",
        &["run", "graph", "check-order", "repeat"],
    ),
    (
        "-order <asked|reversed|finished|shuffled[:seed]>",
        &["run", "graph", "repeat"],
    ),
    (
        "-output <path>",
        &["export", "lock-environment", "telemetry", "report"],
    ),
    ("-platform <name>", &["lock-environment"]),
    ("-license <url>", &["export"]),
    ("-resume [run]", &["run", "graph"]),
    ("-reason <text>", &["reconcile"]),
    ("-strict-provenance", &["run"]),
    ("-page", &["run", "repeat"]),
    ("-address <ip>", &["run", "repeat", "page", "serve"]),
    ("-port <number>", &["run", "repeat", "page", "serve"]),
    ("-keep-days <n>", &["clean"]),
    ("-keep-run <run>", &["clean"]),
    ("-keep-slower-than <time>", &["clean"]),
    ("-remove", &["clean"]),
    ("-grace <time>", &["clean"]),
    ("-front", &["clean"]),
    (
        "-check-reproducibility <percent>",
        &["run", "graph", "check-order", "repeat", "replay"],
    ),
    ("-no-cache", &["run", "graph", "check-order"]),
    ("-no-cache-steps <steps>", &["run", "graph", "check-order"]),
    ("-no-network", &["run", "graph", "check-order", "replay"]),
    ("-machine-retries <n>", EXECUTION),
    ("-machine-retry-delay-ms <milliseconds>", EXECUTION),
    (
        "-task-minutes <minutes>",
        &["run", "graph", "check-order", "repeat"],
    ),
    (
        "-no-right-sizing",
        &["run", "graph", "check-order", "replay"],
    ),
    ("-no-grouping", &["run", "graph", "check-order", "replay"]),
];
