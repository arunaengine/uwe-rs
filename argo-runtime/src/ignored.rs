//! Fields that only tell Kubernetes where and how to run a pod. They do not change what a task
//! computes, so a run without Kubernetes leaves them out with a warning (decision 18).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Node, Value as Yaml};

use crate::run::Run;

/// Fields of workflows, templates and containers that only tell Kubernetes where and how to run
/// a pod; they do not change what a task computes.
const LEFT_OUT: [&str; 26] = [
    "podSpecPatch",
    "nodeSelector",
    "tolerations",
    "affinity",
    "securityContext",
    "serviceAccountName",
    "automountServiceAccountToken",
    "priorityClassName",
    "podPriorityClassName",
    "schedulerName",
    "hostAliases",
    "dnsConfig",
    "dnsPolicy",
    "hostNetwork",
    "imagePullSecrets",
    "imagePullPolicy",
    "podGC",
    "podMetadata",
    "podDisruptionBudget",
    "ttlStrategy",
    "metrics",
    "archiveLocation",
    "artifactGC",
    "volumeClaimGC",
    "executor",
    "initContainers",
];

impl Run {
    /// Warns once about each field of a workflow spec, a template or a container that a run
    /// without Kubernetes leaves out.
    pub(crate) fn warn_left_out(&mut self, node: &Node) {
        for field in LEFT_OUT {
            if node
                .get(field)
                .is_some_and(|value| value.value != Yaml::Null)
            {
                self.warn(format!(
                    "{field} is left out: it only tells Kubernetes how to run pods (decision 18)"
                ));
            }
        }
        for (field, what) in [
            ("memoize", "memoize is left out: every task runs"),
            (
                "synchronization",
                "synchronization is left out: one run holds no locks",
            ),
        ] {
            if node
                .get(field)
                .is_some_and(|value| value.value != Yaml::Null)
            {
                self.warn(what.to_owned());
            }
        }
    }
}
