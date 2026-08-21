//! Fields that only tell Kubernetes where and how to run a pod. They do not change what a task
//! computes, so a run without Kubernetes leaves them out with a warning (decision 18).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Node, Value as Yaml};

use crate::run::Run;

/// Fields of workflows, templates and containers that only tell Kubernetes where and how to run
/// a pod; they do not change what a task computes.
const LEFT_OUT: [&str; 24] = [
    "podSpecPatch",
    "nodeSelector",
    "tolerations",
    "affinity",
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
        // A container runs as the user and group that its `securityContext` names, as in
        // Kubernetes, but with the least permissions (decision 28): it gets no more.
        if let Some(Yaml::Mapping(fields)) = node.get("securityContext").map(|value| &value.value) {
            for field in fields {
                let key = &field.key;
                if key != "runAsUser" && key != "runAsGroup" {
                    self.warn(format!(
                        "securityContext.{key} is left out: containers run with the least \
                         permissions, only runAsUser and runAsGroup count (decision 28)"
                    ));
                }
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
