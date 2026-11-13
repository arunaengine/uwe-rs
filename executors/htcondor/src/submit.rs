//! Writes the submit description of the HTCondor job of an attempt.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use slurm_executor::grid::Submission;

/// The submit description of an attempt, beside its job script.
pub const SUBMIT_FILE: &str = "job.condor";

/// HTCondor's event log of the job of an attempt, beside its job script.
pub const EVENT_LOG: &str = "condor.log";

/// Both streams of the job of an attempt, beside its job script.
pub const JOB_LOG: &str = "condor.out";

/// The submit description of the job of an attempt: its script, logs, file transfer, resources
/// and time, the cluster options of its task, then `lines`, which come last and so win.
pub fn description(submission: &Submission<'_>, lines: &[String]) -> String {
    let control = submission.script.parent().unwrap_or(submission.root);
    let placement = submission.job.placement;
    let path = |name: &str| control.join(name).display().to_string();
    let transfer = match submission.remote {
        true => "YES",
        false => "IF_NEEDED",
    };
    let mut text = vec![
        "universe = vanilla".to_owned(),
        format!("executable = {}", submission.script.display()),
        format!("log = {}", path(EVENT_LOG)),
        // The same file for both streams makes HTCondor merge them.
        format!("output = {}", path(JOB_LOG)),
        format!("error = {}", path(JOB_LOG)),
        format!("initialdir = {}", submission.root.display()),
        format!("should_transfer_files = {transfer}"),
        "when_to_transfer_output = ON_EXIT".to_owned(),
    ];
    if placement.cpus > 1 {
        text.push(format!("request_cpus = {}", placement.cpus));
        text.push("machine_count = 1".to_owned());
    }
    if let Some(bytes) = placement.memory_bytes {
        text.push(format!("request_memory = {}", bytes.div_ceil(1 << 20)));
    }
    if let Some(bytes) = placement.disk_bytes {
        text.push(format!("request_disk = {}", bytes.div_ceil(1 << 10)));
    }
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    if gpus > 0 {
        text.push(format!("request_gpus = {gpus}"));
    }
    if let Some(seconds) = placement.time_seconds {
        text.push(format!(
            "periodic_remove = (RemoteWallClockTime - CumulativeSuspensionTime) > {}",
            seconds.saturating_add(submission.margin)
        ));
    }
    if let Some(proxy) = submission.proxy {
        let names = ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"];
        let pairs: Vec<String> = names.iter().map(|name| format!("{name}={proxy}")).collect();
        text.push(format!("environment = \"{}\"", pairs.join(" ")));
    }
    let name = submission.job.name.replace('"', "");
    text.push(format!("+uwe_attempt = \"{name}\""));
    // A text of cluster options holds lines split by `;`, as nf-ospool takes them.
    match placement.options.get("clusterOptions") {
        Some(serde_json::Value::String(options)) => text.extend(
            (options.split([';', '\n']).map(str::trim))
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        ),
        Some(serde_json::Value::Array(options)) => {
            text.extend(
                options
                    .iter()
                    .filter_map(|line| Some(line.as_str()?.to_owned())),
            );
        }
        _ => {}
    }
    text.extend(lines.iter().cloned());
    text.push("queue".to_owned());
    text.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use executor::{Job, Staging};
    use task_model::{FORMAT, Placement, TaskSpec};

    use super::*;

    /// The pairs of a submit description, with sizes of nf-ospool's text form in uwe's units.
    fn pairs(text: &str) -> Vec<(String, String)> {
        let unit = |value: &str, per_gb: u64| match value.strip_suffix(" GB") {
            Some(gb) => (gb.parse::<u64>().unwrap_or_default() * per_gb).to_string(),
            None => value.to_owned(),
        };
        (text.lines())
            .map(|line| match line.split_once(" = ") {
                Some(("request_memory", value)) => ("request_memory".into(), unit(value, 1 << 10)),
                Some(("request_disk", value)) => ("request_disk".into(), unit(value, 1 << 20)),
                Some((key, _)) if matches!(key, "executable" | "log") => (key.into(), "".into()),
                Some((key, value)) => (key.into(), value.into()),
                None => (line.into(), String::new()),
            })
            .collect()
    }

    #[test]
    fn matches_ospool_submit() {
        let spec = TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: None,
            arguments: Vec::new(),
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
            run_as: None,
        };
        let mut placement = Placement {
            cpus: 2,
            memory_bytes: Some(4 << 30),
            disk_bytes: Some(10 << 30),
            time_seconds: Some(3600),
            ..Placement::default()
        };
        let options = serde_json::json!("HasCHTCStaging=true; +ProjectName = \"probe\"");
        placement.options.insert("clusterOptions".into(), options);
        let staging = Staging::default();
        let job = Job {
            name: "run-BIG-1-1",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let submission = Submission {
            job,
            root: Path::new("/scratch/run-BIG-1-1"),
            script: Path::new("/scratch/run-BIG-1-1/control/job.sh"),
            remote: false,
            margin: 0,
            proxy: None,
        };
        let ours = pairs(&description(&submission, &["+Lab = 1".into()]));
        let recorded = pairs(include_str!("../tests/fixtures/nf_ospool_submit.condor"));
        // Every line of nf-ospool comes in its order, with uwe's own lines between them.
        let mut rest = ours.iter();
        for pair in &recorded {
            assert!(rest.any(|ours| ours == pair), "{pair:?} in {ours:?}");
        }
        assert_eq!(ours[ours.len() - 2], ("+Lab".into(), "1".into()));
        assert!(ours.contains(&("+uwe_attempt".into(), "\"run-BIG-1-1\"".into())));
    }
}
