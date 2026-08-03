//! The executors that the config file or the command line sets up: the TES server, the tools
//! of SLURM and the kubeconfig of a Kubernetes cluster.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use super::{Report, State};
use crate::arguments::Arguments;
use crate::setup::Executors;
use crate::tes;

/// The TES server that `-tes-server` or the config file names, or the default one with
/// `-executor tes`, asked once whether it answers.
pub(super) fn tes_server(executors: &Executors, arguments: &Arguments, report: &mut Report) {
    let part = "executor tes";
    let asked = arguments.executor.as_deref() == Some("tes");
    let server = (arguments.tes_server.clone())
        .or_else(|| executors.tes_server.clone())
        .or_else(|| asked.then(|| tes::DEFAULT_SERVER.to_owned()));
    let Some(server) = server else {
        report.add(
            State::NotSetUp,
            part,
            "no TES server named; name one with -tes-server or `server` in [executor.tes]",
        );
        return;
    };
    let token = match std::env::var("IUNCTUS_TES_TOKEN") {
        Ok(token) if !token.is_empty() => Some(token),
        _ => (executors.tes_token.as_ref()).and_then(|secret| secret.reveal().ok()),
    };
    let answer = tes_executor::Client::new(&server, token)
        .map_err(|error| error.to_string())
        .and_then(|client| tes::answers(&client, &server));
    match answer {
        Ok(()) => report.add(
            State::Ready,
            part,
            format!("the TES server at {server} answers"),
        ),
        Err(problem) => report.add(State::Missing, part, problem),
    }
}

/// The tools of SLURM, which the SLURM executor runs on this machine.
pub(super) fn slurm(executors: &Executors, arguments: &Arguments, report: &mut Report) {
    let part = "executor slurm";
    let asked = arguments.executor.as_deref() == Some("slurm")
        || executors.slurm_container.is_some()
        || executors.slurm_options.is_some();
    let missing: Vec<&str> = (slurm_executor::TOOLS.into_iter())
        .filter(|tool| !slurm_executor::executable(Path::new(tool)))
        .collect();
    let needed: Vec<&str> = missing
        .iter()
        .copied()
        .filter(|tool| *tool != "sacct")
        .collect();
    match (needed.is_empty(), asked) {
        (true, _) if missing.is_empty() => report.add(
            State::Ready,
            part,
            "sbatch, squeue, sacct and scancel are on the PATH",
        ),
        (true, _) => report.add(
            State::Ready,
            part,
            "sbatch, squeue and scancel are on the PATH; without sacct, a job that SLURM has \
             forgotten ends as a failure of the machine",
        ),
        (false, true) => report.add(
            State::Missing,
            part,
            format!(
                "{} not on the PATH; run iunctus on a node of the SLURM cluster, like its login \
                 node",
                tools(&needed)
            ),
        ),
        (false, false) => report.add(
            State::NotSetUp,
            part,
            "the tools of SLURM are not on the PATH; -executor slurm runs on a node of a SLURM \
             cluster",
        ),
    }
}

/// `a is`, `a and b are`, `a, b and c are`.
fn tools(names: &[&str]) -> String {
    match names.split_last() {
        Some((one, [])) => format!("`{one}` is"),
        Some((last, others)) => format!("`{}` and `{last}` are", others.join("`, `")),
        None => String::new(),
    }
}

/// The kubeconfig that `-kubeconfig` or the config file names, read without asking the cluster.
pub(super) fn kubernetes(
    runtime: &tokio::runtime::Runtime,
    executors: &Executors,
    arguments: &Arguments,
    report: &mut Report,
) {
    let part = "executor kubernetes";
    let kubeconfig = (arguments.kubeconfig.clone()).or_else(|| executors.kubeconfig.clone());
    let Some(kubeconfig) = kubeconfig else {
        report.add(
            State::NotSetUp,
            part,
            "no kubeconfig named; name one with -kubeconfig or `kubeconfig` in \
             [executor.kubernetes]; iunctus never takes the default one",
        );
        return;
    };
    let namespace =
        (arguments.kubernetes_namespace.clone()).or_else(|| executors.kubernetes_namespace.clone());
    if !kubeconfig.is_file() {
        report.add(
            State::Missing,
            part,
            format!("the kubeconfig {} does not exist", kubeconfig.display()),
        );
        return;
    }
    let connected = runtime.block_on(kubernetes_executor::Cluster::connect(
        &kubeconfig,
        namespace,
    ));
    match connected {
        Ok(cluster) => report.add(
            State::Ready,
            part,
            format!(
                "the kubeconfig {} is readable; Jobs go to the namespace {}",
                kubeconfig.display(),
                cluster.namespace()
            ),
        ),
        Err(error) => report.add(
            State::Missing,
            part,
            error.lines().next().unwrap_or_default().to_owned(),
        ),
    }
}
