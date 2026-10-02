//! Resolves task network access from workflow requirements and run policy.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Network, Task};

/// Which tasks of a run get the network.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkRule {
    /// Tasks whose workflow does not say run without network.
    pub none_by_default: bool,
    /// Steps that get the network in any case: a name of a step, or of its last part after
    /// `:`, like `FASTQC` for `NFCORE_DEMO:DEMO:FASTQC`; `*` stands for any text.
    pub allowed_steps: Vec<String>,
}

impl NetworkRule {
    /// Whether a task runs without network.
    pub fn offline(&self, task: &Task) -> bool {
        if self.allowed(&task.step) {
            return false;
        }
        match task.network {
            Network::Allowed => false,
            Network::Denied => true,
            Network::Unstated => self.none_by_default,
        }
    }

    /// Whether the config file lets a step use the network.
    fn allowed(&self, step: &str) -> bool {
        let last = step.rsplit(':').next().unwrap_or(step);
        (self.allowed_steps.iter()).any(|pattern| matches(pattern, step) || matches(pattern, last))
    }
}

/// Whether a name matches a pattern in which `*` stands for any text.
fn matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(step: &str, network: Network) -> Task {
        Task {
            step: step.into(),
            network,
            ..Task::default()
        }
    }

    #[test]
    fn selects_result_destinations() {
        let open = NetworkRule::default();
        assert!(!open.offline(&task("A", Network::Unstated)));
        assert!(open.offline(&task("A", Network::Denied)));
        let closed = NetworkRule {
            none_by_default: true,
            allowed_steps: vec!["DOWNLOAD_*".into(), "fetch".into()],
        };
        assert!(closed.offline(&task("A", Network::Unstated)));
        assert!(!closed.offline(&task("A", Network::Allowed)));
        assert!(!closed.offline(&task("NFCORE:DOWNLOAD_GENOME", Network::Unstated)));
        assert!(!closed.offline(&task("fetch", Network::Denied)));
        assert!(closed.offline(&task("prefetch", Network::Unstated)));
    }

    #[test]
    fn stars_match_text() {
        assert!(matches("*", "anything"));
        assert!(matches("a*c", "abbbc"));
        assert!(matches("*QC*", "FASTQC_RAW"));
        assert!(!matches("a*c", "abd"));
        assert!(matches("exact", "exact"));
        assert!(!matches("exact", "exactly"));
    }
}
