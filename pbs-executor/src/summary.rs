//! Finds the resource summary that some PBS sites, like NCI Gadi, append to the log of a job.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The last resource summary in a job log: the lines from a rule of `=` before a line with
/// `Resource Usage` to the next rule, with a final newline.
pub fn summary(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let rule = |line: &str| line.len() >= 20 && line.trim_end().bytes().all(|byte| byte == b'=');
    let title = (lines.iter()).rposition(|line| line.contains("Resource Usage"))?;
    let start = lines[..title].iter().rposition(|line| rule(line))?;
    let end = title + lines[title..].iter().position(|line| rule(line))?;
    Some(lines[start..=end].join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_gadi_summary() {
        let block = include_str!("../tests/fixtures/nf_gadi_job.log");
        let log = format!("big\n{block}");
        assert_eq!(summary(&log).as_deref(), Some(block));
        assert_eq!(summary("big\n"), None);
        let cut = block
            .rsplit_once("====")
            .map(|(start, _)| start)
            .unwrap_or_default();
        assert_eq!(summary(cut.trim_end_matches('=')), None);
    }
}
