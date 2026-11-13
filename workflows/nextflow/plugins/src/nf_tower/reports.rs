//! The reports of `tower.yml`: published files that match its patterns, listed in a TSV file
//! for Seqera Platform.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write as _;
use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::glob::matches;
use nextflow_evaluator::interpreter::parse_yaml;
use nextflow_evaluator::value::Value;

use crate::observer::Start;

/// The reports file and the patterns of `tower.yml`, in their order there.
pub(super) struct Reports {
    file: PathBuf,
    entries: Vec<Entry>,
}

struct Entry {
    key: String,
    display: String,
    mime: String,
}

impl Reports {
    /// The reports of `nf-<id>-tower.yml` in the launch directory, which Platform writes,
    /// else of `tower.yml` in the project; `None` without either or without `reports`.
    pub(super) fn load(
        start: &Start,
        workflow_id: &str,
        file_name: Option<&str>,
    ) -> Result<Option<Reports>, String> {
        let launch = Path::new(&start.launch_dir);
        let candidates = [
            launch.join(format!("nf-{workflow_id}-tower.yml")),
            Path::new(&start.project_dir).join("tower.yml"),
        ];
        let Some(text) = candidates
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
        else {
            return Ok(None);
        };
        let invalid = |reason: String| format!("Invalid tower.yml format -- {reason}");
        let yaml = parse_yaml(&text).map_err(|error| invalid(error.message.clone()))?;
        let reports = match &yaml {
            Value::Map(map) => (map.borrow().entries.get(&key("reports"))).cloned(),
            _ => None,
        };
        let Some(Value::Map(reports)) = reports else {
            return Ok(None);
        };
        let field = |settings: &Value, name: &str| match settings {
            Value::Map(map) => (map.borrow().entries.get(&key(name)))
                .filter(|value| !matches!(value, Value::Null))
                .map_or(String::new(), Value::to_string),
            _ => String::new(),
        };
        let entries = (reports.borrow().entries.iter())
            .map(|(name, settings)| Entry {
                key: name.0.to_string(),
                display: field(settings, "display"),
                mime: field(settings, "mimeType"),
            })
            .collect();
        let name = file_name.map_or(format!("nf-{workflow_id}-reports.tsv"), str::to_owned);
        let file = launch.join(name);
        std::fs::write(&file, "key\tpath\tsize\tdisplay\tmime_type\n")
            .map_err(|error| invalid(error.to_string()))?;
        Ok(Some(Reports { file, entries }))
    }

    /// Lists a published file under the first pattern it matches; a pattern that does not
    /// start with `**/` matches at any depth.
    pub(super) fn publish(&mut self, target: &str) {
        let found = self.entries.iter().find(|entry| {
            let pattern = match entry.key.starts_with("**/") {
                true => entry.key.clone(),
                false => format!("**/{}", entry.key),
            };
            matches(&pattern, target)
        });
        let Some(entry) = found else {
            return;
        };
        let size = std::fs::metadata(target).map_or(0, |metadata| metadata.len());
        let line = format!(
            "{}\t{target}\t{size}\t{}\t{}\n",
            entry.key, entry.display, entry.mime
        );
        let written = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.file)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(error) = written {
            tracing::warn!(%error, "the Seqera Platform reports file was not written");
        }
    }
}

fn key(name: &str) -> nextflow_evaluator::value::Key {
    nextflow_evaluator::value::Key(Value::string(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_matching_files() {
        let directory = tempfile::tempdir().expect("a folder");
        let folder = directory.path().to_string_lossy().into_owned();
        let tower = "reports:\n  \"report.html\":\n    display: \"Joined report\"\n    \
                     mimeType: \"text/html\"\n  \"*.txt\":\n    display: \"Greeting\"\n";
        std::fs::write(directory.path().join("tower.yml"), tower).expect("writes tower.yml");
        let results = directory.path().join("results");
        std::fs::create_dir(&results).expect("makes results");
        std::fs::write(results.join("a.txt"), "hello a\n").expect("writes a");
        std::fs::write(results.join("report.html"), "x").expect("writes the report");
        let start = Start {
            launch_dir: folder.clone(),
            project_dir: folder.clone(),
            ..crate::run_metadata::tests::start()
        };
        let mut reports = Reports::load(&start, "abc", None)
            .expect("reads tower.yml")
            .expect("has reports");
        for name in ["a.txt", "report.html", "other.csv"] {
            reports.publish(&format!("{folder}/results/{name}"));
        }
        let text = std::fs::read_to_string(directory.path().join("nf-abc-reports.tsv"))
            .expect("the reports file");
        assert_eq!(
            text,
            format!(
                "key\tpath\tsize\tdisplay\tmime_type\n*.txt\t{folder}/results/a.txt\t8\t\
                 Greeting\t\nreport.html\t{folder}/results/report.html\t1\tJoined report\t\
                 text/html\n"
            )
        );
    }
}
