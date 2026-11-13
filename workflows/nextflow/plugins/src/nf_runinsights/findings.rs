//! The findings of nf-runinsights: each process of a run against the earlier runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::stats::{Prior, Stats, bytes, duration, median};
use crate::report_format::Json;

/// Something about a process of this run worth telling, against the history.
pub(super) struct Finding {
    pub(super) kind: &'static str,
    pub(super) severity: &'static str,
    pub(super) process: String,
    pub(super) message: String,
}

impl Finding {
    pub(super) fn json(&self) -> Json {
        Json::Object(Vec::new())
            .with("type", self.kind)
            .with("severity", self.severity)
            .with("process", self.process.as_str())
            .with("message", self.message.as_str())
    }
}

/// The median realtime of a process over the earlier runs that ran it.
pub(super) fn history_median(process: &str, prior: &[Prior]) -> Option<f64> {
    let stats = prior.iter().filter_map(|run| run.stats(process));
    median(stats.filter_map(|stats| stats.realtime_median).collect())
}

/// The findings of each process, in the order of the rules, after a pipeline-wide slowdown.
pub(super) fn findings(current: &[(String, Stats)], prior: &[Prior]) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut ratios = Vec::new();
    for (process, stats) in current {
        let history: Vec<&Stats> = prior.iter().filter_map(|run| run.stats(process)).collect();
        let base = median(history.iter().filter_map(|it| it.realtime_median).collect());
        let finding = |kind, severity, message: String| Finding {
            kind,
            severity,
            process: process.clone(),
            message: format!("{process}: {message}"),
        };
        if let (Some(now), Some(base)) = (stats.realtime_median, base.filter(|base| *base > 0.0)) {
            let ratio = now / base;
            ratios.push(ratio);
            let (now_text, base_text) = (duration(Some(now)), duration(Some(base)));
            if ratio >= 1.5 && now - base >= 2000.0 {
                let mut message = format!(
                    "{now_text} vs {base_text} median over {} prior run(s), {ratio:.1}x slower",
                    history.len()
                );
                let causes = causes(stats, &history, now - base, ratio);
                if !causes.is_empty() {
                    message.push_str(&format!(", likely cause: {}", causes.join("; ")));
                }
                found.push(finding("regression", "warn", message));
            } else if ratio <= 0.66 && base - now >= 2000.0 {
                let message = format!(
                    "{now_text} vs {base_text} median, {:.1}x faster",
                    1.0 / ratio
                );
                found.push(finding("improvement", "info", message));
            }
        }
        if stats.retried > 0 {
            let message = format!(
                "{} of {} task(s) needed retries, failures inflate wall time and cost",
                stats.retried, stats.tasks
            );
            found.push(finding("retries", "warn", message));
        }
        let peaks: Vec<f64> = std::iter::once(stats)
            .chain(history.iter().copied())
            .filter_map(|stats| stats.peak_rss_max)
            .collect();
        let peak = peaks.iter().copied().reduce(f64::max);
        if let (Some(memory), Some(peak)) = (stats.memory.filter(|it| *it > 0.0), peak)
            && peak < memory * 0.25
        {
            let message = format!(
                "peak memory {} never exceeded 25% of the {} requested ({} sample(s))",
                bytes(Some(peak)),
                bytes(Some(memory)),
                peaks.len()
            );
            found.push(finding("overprovision", "info", message));
        }
        if stats.failed > 0 {
            let message = format!(
                "{} of {} task(s) did not complete",
                stats.failed, stats.tasks
            );
            found.push(finding("failures", "warn", message));
        }
    }
    let slower = ratios.iter().filter(|ratio| **ratio >= 1.3).count();
    if ratios.len() >= 4 && slower >= (ratios.len() * 3).div_ceil(5) {
        let message = format!(
            "pipeline-wide slowdown: {slower} of {} processes are >=1.3x slower than history, \
             this points at the machine, cluster load, or storage, not at any single tool",
            ratios.len()
        );
        let environment = Finding {
            kind: "environment",
            severity: "warn",
            process: "*".into(),
            message,
        };
        found.insert(0, environment);
    }
    found
}

/// The likely causes of a regression: queue wait, a new container, more input.
fn causes(stats: &Stats, history: &[&Stats], increase: f64, ratio: f64) -> Vec<String> {
    let mut causes = Vec::new();
    let queue = median(history.iter().filter_map(|it| it.queue_median).collect());
    if let (Some(now), Some(before)) = (stats.queue_median, queue) {
        let grown = now - before;
        if grown >= increase / 2.0 && grown > 0.0 {
            causes.push("mostly queue wait (scheduler/cluster load), not slower execution".into());
        }
    }
    let before = history.iter().rev().find_map(|it| it.container.as_ref());
    if let (Some(before), Some(now)) = (before, &stats.container)
        && before != now
    {
        causes.push(format!("container changed ({before} \u{2192} {now})"));
    }
    let reads = median(history.iter().filter_map(|it| it.read_total).collect());
    if let (Some(now), Some(before)) = (stats.read_total, reads.filter(|it| *it > 0.0)) {
        let grown = now / before;
        if grown > 1.2 && grown >= 0.7 * ratio {
            causes.push(format!(
                "input data grew {grown:.1}x, throughput is roughly flat, so this is growth, \
                 not a regression"
            ));
        }
    }
    causes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(median: f64) -> Stats {
        Stats {
            tasks: 2,
            realtime_median: Some(median),
            memory: Some(2147483648.0),
            ..Stats::default()
        }
    }

    #[test]
    fn finds_regressions() {
        // The recorded second run: 3982 ms against the 977 ms of the first.
        let prior = Prior {
            ts: "2026-11-06T10:45:09.841+01:00".into(),
            run_name: "spontaneous_goldwasser".into(),
            pipeline: "ri/main.nf".into(),
            completed: true,
            processes: vec![("CRUNCH".into(), stats(977.0))],
        };
        let found = findings(&[("CRUNCH".into(), stats(3982.0))], &[prior]);
        let messages: Vec<&str> = found.iter().map(|it| it.message.as_str()).collect();
        assert_eq!(
            messages,
            ["CRUNCH: 4.0s vs 1.0s median over 1 prior run(s), 4.1x slower"]
        );
        assert_eq!((found[0].kind, found[0].severity), ("regression", "warn"));
    }
}
