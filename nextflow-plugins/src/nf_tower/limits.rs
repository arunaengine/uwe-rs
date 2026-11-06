//! The longest text that the Platform takes for each request field; longer text is cut.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::report_format::Json;

/// Fields by their path of keys joined with `.`, where list items add no level.
const LIMITS: &[(&str, usize)] = &[
    ("workflowId", 16),
    ("workflow.id", 16),
    ("workflow.commitId", 40),
    ("workflow.userName", 40),
    ("workflow.scriptId", 40),
    ("workflow.sessionId", 36),
    ("workflow.runName", 80),
    ("workflow.profile", 100),
    ("workflow.revision", 100),
    ("workflow.projectName", 100),
    ("workflow.scriptName", 100),
    ("workflow.operationId", 110),
    ("workflow.projectDir", 255),
    ("workflow.homeDir", 255),
    ("workflow.workDir", 255),
    ("workflow.container", 255),
    ("workflow.errorMessage", 255),
    ("workflow.repository", 255),
    ("workflow.containerEngine", 255),
    ("workflow.scriptFile", 255),
    ("workflow.launchDir", 255),
    ("workflow.outFile", 255),
    ("workflow.logFile", 255),
    ("workflow.nextflow.version", 20),
    ("workflow.nextflow.build", 10),
    ("workflow.manifest.nextflowVersion", 20),
    ("workflow.manifest.defaultBranch", 20),
    ("workflow.manifest.version", 20),
    ("workflow.manifest.homePage", 200),
    ("workflow.manifest.description", 1024),
    ("workflow.manifest.name", 150),
    ("workflow.manifest.mainScript", 100),
    ("workflow.manifest.author", 150),
    ("workflow.manifest.gitmodules", 150),
    ("workflow.manifest.doi", 100),
    ("workflow.stats.computeTimeFmt", 50),
    ("processNames", 255),
    ("tasks.env", 2048),
    ("tasks.script", 10240),
    ("tasks.status", 10),
    ("tasks.errorAction", 10),
    ("tasks.hash", 34),
    ("tasks.name", 255),
    ("tasks.process", 255),
    ("tasks.tag", 255),
    ("tasks.container", 255),
    ("tasks.scratch", 255),
    ("tasks.module", 255),
    ("tasks.workdir", 512),
    ("tasks.queue", 100),
    ("tasks.nativeId", 100),
    ("tasks.executor", 25),
    ("tasks.machineType", 25),
    ("tasks.cloudZone", 25),
    ("tasks.priceModel", 15),
    ("progress.name", 255),
    ("metrics.process", 255),
];

/// The labels of the metric summaries, cut at 255 characters in every series.
const LABELS: [&str; 5] = ["minLabel", "maxLabel", "q1Label", "q2Label", "q3Label"];

/// Cuts the text values of a request body that are longer than the Platform takes.
pub(super) fn limit(body: &mut Json) {
    walk(body, "");
}

fn walk(value: &mut Json, path: &str) {
    match value {
        Json::Object(entries) => {
            for (key, value) in entries {
                let inner = match path {
                    "" => key.clone(),
                    _ => format!("{path}.{key}"),
                };
                walk(value, &inner);
            }
        }
        Json::List(items) => items.iter_mut().for_each(|item| walk(item, path)),
        Json::Text(text) => {
            let Some(max) = longest(path) else {
                return;
            };
            let size = text.chars().count();
            if size > max {
                let shown: String = text.chars().take(100).collect();
                tracing::warn!(
                    "Seqera Platform request field `{path}` exceeds expected size | offending \
                     value: `{shown}(truncated)`, size: {size} (max: {max})"
                );
                *text = text.chars().take(max).collect();
            }
        }
        _ => {}
    }
}

fn longest(path: &str) -> Option<usize> {
    if let Some(found) = LIMITS.iter().find(|(name, _)| *name == path) {
        return Some(found.1);
    }
    let label = path.rsplit('.').next().unwrap_or_default();
    (path.starts_with("metrics.") && LABELS.contains(&label)).then_some(255)
}
