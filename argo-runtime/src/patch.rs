//! Applies `podSpecPatch` to the containers and volumes of a template, as Argo's strategic merge does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Entry, Node, Value as Yaml, parse_yaml};

use crate::run::{Failure, Result, Run, entry_mut, scalar_text};

/// The key that names the patches of a template: the one of the workflow, then its own.
pub(crate) const PATCHES: &str = "podSpecPatch";

/// The key of each list of a pod spec or a container that a strategic merge joins by item.
const MERGE_KEYS: [(&str, &str); 6] = [
    ("containers", "name"),
    ("initContainers", "name"),
    ("volumes", "name"),
    ("env", "name"),
    ("volumeMounts", "mountPath"),
    ("ports", "containerPort"),
];

/// Pod fields that a patch may set and that only tell Kubernetes how to run the pod.
const LEFT_OUT: [&str; 16] = [
    "nodeSelector",
    "tolerations",
    "affinity",
    "serviceAccountName",
    "priorityClassName",
    "schedulerName",
    "hostAliases",
    "dnsConfig",
    "dnsPolicy",
    "imagePullSecrets",
    "runtimeClassName",
    "terminationGracePeriodSeconds",
    "topologySpreadConstraints",
    "securityContext",
    "activeDeadlineSeconds",
    "restartPolicy",
];

impl Run {
    /// Merges the patches of a substituted template into its containers, init containers and
    /// volumes; the containers of Argo's own, `wait` and `init`, do not exist here.
    pub(crate) fn apply_patches(&mut self, template: &mut Node) -> Result<()> {
        let patches = match template.get(PATCHES).map(|node| &node.value) {
            Some(Yaml::Sequence(items)) => items.clone(),
            Some(Yaml::Null) | None => return Ok(()),
            Some(_) => vec![template.get(PATCHES).cloned().unwrap()],
        };
        crate::run::remove_key(template, PATCHES);
        for patch in patches {
            let text = scalar_text(&patch);
            let parsed = parse_yaml(&text)
                .map_err(|error| Failure::error(format!("podSpecPatch: {}", error.message)))?;
            let Some(Node {
                value: Yaml::Mapping(fields),
                ..
            }) = parsed
            else {
                continue;
            };
            for field in fields {
                match field.key.as_str() {
                    "containers" => self.patch_containers(template, &field.value)?,
                    "initContainers" | "volumes" => merge_field(template, &field)?,
                    key if LEFT_OUT.contains(&key) => self.warn(format!(
                        "podSpecPatch.{key} is left out: it only tells Kubernetes how to run pods (decision 18)"
                    )),
                    key => {
                        return Err(Failure::unsupported(format!(
                            "podSpecPatch.{key} is not supported"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn patch_containers(&mut self, template: &mut Node, containers: &Node) -> Result<()> {
        let Yaml::Sequence(items) = &containers.value else {
            return Err(Failure::error("podSpecPatch.containers must be a list"));
        };
        for item in items {
            let name = item.get("name").map(scalar_text).unwrap_or_default();
            let target = match name.as_str() {
                "main" => ["container", "script"]
                    .into_iter()
                    .find(|key| template.get(key).is_some())
                    .and_then(|key| entry_mut(template, key)),
                _ => entry_mut(template, "containerSet")
                    .and_then(|set| entry_mut(set, "containers"))
                    .and_then(|list| match &mut list.value {
                        Yaml::Sequence(items) => (items.iter_mut()).find(|item| {
                            item.get("name").map(scalar_text).as_deref() == Some(&name)
                        }),
                        _ => None,
                    }),
            };
            match target {
                Some(target) => merge(target, item)?,
                None => self.warn(format!(
                    "podSpecPatch container {name} is left out: the task has no container of that name"
                )),
            }
        }
        Ok(())
    }
}

/// Merges a patch into a value: maps by key, lists of [`MERGE_KEYS`] by their item key, and
/// anything else by replacing it.
fn merge(target: &mut Node, patch: &Node) -> Result<()> {
    let (Yaml::Mapping(_), Yaml::Mapping(fields)) = (&target.value, &patch.value) else {
        *target = patch.clone();
        return Ok(());
    };
    for field in fields {
        merge_field(target, field)?;
    }
    Ok(())
}

fn merge_field(target: &mut Node, field: &Entry) -> Result<()> {
    let Yaml::Mapping(entries) = &mut target.value else {
        return Ok(());
    };
    let Some(existing) = entries.iter_mut().find(|entry| entry.key == field.key) else {
        entries.push(field.clone());
        return Ok(());
    };
    let key = MERGE_KEYS.iter().find(|(list, _)| *list == field.key);
    match (key, &mut existing.value.value, &field.value.value) {
        (Some((_, item_key)), Yaml::Sequence(items), Yaml::Sequence(patches)) => {
            for patch in patches {
                let wanted = patch.get(item_key).map(scalar_text);
                let found = items
                    .iter_mut()
                    .find(|item| wanted.is_some() && item.get(item_key).map(scalar_text) == wanted);
                match found {
                    Some(item) => merge(item, patch)?,
                    None => items.push(patch.clone()),
                }
            }
            Ok(())
        }
        _ => merge(&mut existing.value, &field.value),
    }
}
