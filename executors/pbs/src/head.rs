//! Writes the `#PBS` lines of the job script of an attempt: its name, log and resources.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use crate::JOB_LOG;
use slurm_executor::grid::Submission;

use crate::Pbs;

/// The `#PBS` lines of an attempt: the cluster options of its task first, as Nextflow writes
/// them, then uwe's lines for what neither they nor the `qsub` options of `pbs` set. A queue
/// or option that could end its line, and so put a command into the job script, is an error.
pub fn head(submission: &Submission<'_>, pbs: &Pbs) -> Result<String, String> {
    let placement = submission.job.placement;
    let cluster: Vec<String> = match placement.options.get("clusterOptions") {
        Some(serde_json::Value::String(text)) => vec![text.trim().to_owned()],
        Some(serde_json::Value::Array(items)) => {
            let words: Vec<&str> = items.iter().filter_map(serde_json::Value::as_str).collect();
            vec![words.join(" ")]
        }
        _ => Vec::new(),
    };
    let cluster: Vec<String> = cluster
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect();
    let words = (cluster.iter().flat_map(|line| line.split_whitespace())).chain(
        pbs.options
            .iter()
            .flat_map(|option| option.split_whitespace()),
    );
    let given = given(words);
    let control = submission.script.parent().unwrap_or(submission.root);
    let log = match submission.remote {
        // The node has no attempt folder; the job sends its log through the staging root.
        true => "/dev/null".to_owned(),
        false => control.join(JOB_LOG).display().to_string(),
    };
    let mut lines: Vec<(&str, String)> = vec![
        ("-N", format!("-N {}", name(submission.job.name))),
        ("-o", format!("-o {log}")),
        ("-j", "-j oe".to_owned()),
    ];
    if let Some(queue) = &placement.queue {
        if !queue_name(queue) {
            return Err(format!(
                "`{}` is not a PBS queue name",
                queue.escape_debug()
            ));
        }
        lines.push(("-q", format!("-q {queue}")));
    }
    if let Some(project) = &pbs.project {
        lines.push(("-P", format!("-P {project}")));
    }
    lines.push(("ncpus", format!("-l ncpus={}", placement.cpus.max(1))));
    if let Some(bytes) = placement.memory_bytes {
        lines.push(("mem", format!("-l mem={}mb", bytes.div_ceil(1 << 20))));
    }
    if let Some(seconds) = placement.time_seconds {
        let seconds = seconds.saturating_add(submission.margin);
        let time = format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        );
        lines.push(("walltime", format!("-l walltime={time}")));
    }
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    if gpus > 0 {
        lines.push(("ngpus", format!("-l ngpus={gpus}")));
    }
    if let (Some(resource), Some(bytes)) = (&pbs.disk_resource, placement.disk_bytes) {
        let size = bytes.div_ceil(1 << 20);
        lines.push((resource.as_str(), format!("-l {resource}={size}mb")));
    }
    if let Some(storage) = &pbs.storage {
        lines.push(("storage", format!("-l storage={storage}")));
    }
    let chunk = given.contains("select");
    let ours = (lines.into_iter())
        .filter(|(key, _)| !given.contains(*key))
        .filter(|(key, _)| !chunk || !matches!(*key, "ncpus" | "mem" | "ngpus"))
        .map(|(_, line)| line);
    let all: Vec<String> = cluster.into_iter().chain(ours).collect();
    if let Some(line) = all.iter().find(|line| line.chars().any(char::is_control)) {
        return Err(format!(
            "the PBS option `{}` holds a line break or another control character",
            line.escape_debug()
        ));
    }
    Ok(all.iter().map(|line| format!("#PBS {line}\n")).collect())
}

/// Whether a queue is a PBS destination, like `normal` or `gpu@server`: letters, digits, `_`,
/// `.`, `-` and `@`, and no leading `-` that `qsub` would read as an option.
fn queue_name(queue: &str) -> bool {
    let mut chars = queue.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-@".contains(c))
}

/// The options and resources that these `qsub` words set: flags like `-q`, and the names of
/// the resources of `-l`, like `ncpus` and `select`.
fn given<'a>(words: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let mut given = BTreeSet::new();
    let mut words = words.peekable();
    while let Some(word) = words.next() {
        let Some(flag) = word.get(..2).filter(|flag| flag.starts_with('-')) else {
            continue;
        };
        if flag != "-l" {
            given.insert(flag.to_owned());
            continue;
        }
        let list = match &word[2..] {
            "" => words.next().unwrap_or_default(),
            rest => rest,
        };
        for resource in list.split(',') {
            given.insert(resource.split('=').next().unwrap_or_default().to_owned());
        }
    }
    given
}

/// A job name that PBS takes: `uwe-` and the attempt, in letters, digits, `.`, `_` and `-`.
fn name(attempt: &str) -> String {
    let clean: String = (attempt.chars())
        .map(|c| match c.is_ascii_alphanumeric() || "._-".contains(c) {
            true => c,
            false => '_',
        })
        .take(200)
        .collect();
    format!("uwe-{clean}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use executor::{Job, Staging};
    use task_model::{FORMAT, Placement, TaskSpec};

    use super::*;

    fn spec() -> TaskSpec {
        TaskSpec {
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
        }
    }

    fn lines(placement: &Placement, pbs: &Pbs) -> Vec<String> {
        let (spec, staging) = (spec(), Staging::default());
        let job = Job {
            name: "run-BIG-1-1",
            spec: &spec,
            staging: &staging,
            placement,
        };
        let submission = Submission {
            job,
            root: Path::new("/scratch/run-BIG-1-1"),
            script: Path::new("/scratch/run-BIG-1-1/control/job.sh"),
            remote: false,
            margin: 0,
            proxy: None,
        };
        let head = head(&submission, pbs).expect("a valid head");
        head.lines().map(str::to_owned).collect()
    }

    fn refused(placement: &Placement) -> String {
        let (spec, staging) = (spec(), Staging::default());
        let job = Job {
            name: "run-BIG-1-1",
            spec: &spec,
            staging: &staging,
            placement,
        };
        let submission = Submission {
            job,
            root: Path::new("/scratch/run-BIG-1-1"),
            script: Path::new("/scratch/run-BIG-1-1/control/job.sh"),
            remote: false,
            margin: 0,
            proxy: None,
        };
        head(&submission, &Pbs::default()).expect_err("an injected line is refused")
    }

    #[test]
    fn rejects_line_breaks() {
        let queue = Placement {
            queue: Some("normal\ntouch /tmp/owned".into()),
            ..Placement::default()
        };
        assert!(refused(&queue).contains("not a PBS queue name"));
        for queue in ["-v", "normal queue", "", "q\r"] {
            let placement = Placement {
                queue: Some(queue.into()),
                ..Placement::default()
            };
            refused(&placement);
        }
        for options in [
            serde_json::json!("-P ab12\ntouch /tmp/owned"),
            serde_json::json!("-P ab12\rtouch /tmp/owned"),
            serde_json::json!(["-l", "select=1\ntouch /tmp/owned"]),
            serde_json::json!(["-l\u{0}", "select=1"]),
        ] {
            let mut placement = Placement::default();
            placement.options.insert("clusterOptions".into(), options);
            assert!(refused(&placement).contains("control character"));
        }
    }

    #[test]
    fn takes_queue_destinations() {
        let placement = Placement {
            queue: Some("gpu-a100@pbs.example".into()),
            ..Placement::default()
        };
        let ours = lines(&placement, &Pbs::default());
        assert!(
            ours.contains(&"#PBS -q gpu-a100@pbs.example".to_owned()),
            "{ours:?}"
        );
        assert!(ours.iter().all(|line| line.starts_with("#PBS ")));
    }

    #[test]
    fn matches_gadi_header() {
        let mut placement = Placement {
            cpus: 2,
            memory_bytes: Some(1 << 30),
            time_seconds: Some(3600),
            queue: Some("normal".into()),
            ..Placement::default()
        };
        let options = serde_json::json!("-P ab12 -l storage=gdata/ab12");
        placement.options.insert("clusterOptions".into(), options);
        let ours = lines(&placement, &Pbs::default());
        let recorded = include_str!("../tests/fixtures/nf_gadi_header.txt");
        // Each line of Nextflow's, but uwe's job name and log, is one of uwe's.
        for line in recorded.lines().filter(|line| line.starts_with("#PBS")) {
            let line = match line.split_whitespace().nth(1) {
                Some("-N") => "#PBS -N uwe-run-BIG-1-1",
                Some("-o") => "#PBS -o /scratch/run-BIG-1-1/control/pbs.log",
                _ => line,
            };
            assert!(ours.iter().any(|ours| ours == line), "{line} in {ours:?}");
        }
        assert!(ours.contains(&"#PBS -l ncpus=2".to_owned()));
        assert!(ours.contains(&"#PBS -l mem=1024mb".to_owned()));
    }

    #[test]
    fn leaves_given_resources() {
        let mut placement = Placement {
            cpus: 4,
            memory_bytes: Some(2 << 30),
            disk_bytes: Some(100 << 20),
            ..Placement::default()
        };
        let options = serde_json::json!(["-l", "select=1:ncpus=8:mem=4gb"]);
        placement.options.insert("clusterOptions".into(), options);
        let pbs = Pbs {
            options: vec!["-q express".into()],
            project: Some("ab12".into()),
            storage: Some("gdata/ab12+scratch/ab12".into()),
            disk_resource: Some("jobfs".into()),
            ..Pbs::default()
        };
        let ours = lines(&placement, &pbs);
        assert_eq!(ours[0], "#PBS -l select=1:ncpus=8:mem=4gb");
        for line in [
            "#PBS -P ab12",
            "#PBS -l jobfs=100mb",
            "#PBS -l storage=gdata/ab12+scratch/ab12",
        ] {
            assert!(ours.contains(&line.to_owned()), "{line} in {ours:?}");
        }
        let left = ["-q", "ncpus=", "mem="];
        let mine = &ours[1..];
        assert!(
            !mine
                .iter()
                .any(|line| left.iter().any(|word| line.contains(word))),
            "{ours:?}"
        );
    }
}
