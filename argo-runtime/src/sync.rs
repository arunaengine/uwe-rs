//! Resolves semaphore limits and admits nodes through locks shared by one run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;

use argo_parser::{Node, Value as Yaml};

use crate::library::{Kind, Library};
use crate::node::Phase;
use crate::run::{Failure, Processed, Result, Run, scalar_text};
use crate::tags::{Variables, replace_node};

#[derive(Default)]
pub(crate) struct Locks(BTreeMap<String, Lock>);

struct Lock {
    limit: usize,
    holders: HashSet<String>,
    waiting: VecDeque<String>,
}

impl Locks {
    pub fn acquire(&mut self, owner: &str, limits: &[(String, usize)]) -> Result<bool> {
        for (key, limit) in limits {
            if self.0.get(key).is_some_and(|lock| lock.limit != *limit) {
                return Err(Failure::error(format!(
                    "synchronization {key}: conflicting semaphore limits"
                )));
            }
            let lock = self.0.entry(key.clone()).or_insert_with(|| Lock {
                limit: *limit,
                holders: HashSet::new(),
                waiting: VecDeque::new(),
            });
            if !lock.holders.contains(owner) && !lock.waiting.iter().any(|item| item == owner) {
                lock.waiting.push_back(owner.into());
            }
        }
        if limits.iter().any(|(key, _)| {
            let lock = &self.0[key];
            !lock.holders.contains(owner)
                && (lock.holders.len() >= lock.limit
                    || lock.waiting.front().is_none_or(|item| item != owner))
        }) {
            return Ok(false);
        }
        for (key, _) in limits {
            let lock = self.0.get_mut(key).unwrap();
            if lock.holders.insert(owner.into()) {
                lock.waiting.pop_front();
            }
        }
        Ok(true)
    }

    pub fn release(&mut self, owner: &str) {
        for lock in self.0.values_mut() {
            lock.holders.remove(owner);
            lock.waiting.retain(|item| item != owner);
        }
    }
}

fn entries<'a>(node: &'a Node, plural: &str, single: &str) -> Result<Vec<&'a Node>> {
    match node.get(plural) {
        Some(Node {
            value: Yaml::Sequence(items),
            ..
        }) => Ok(items.iter().collect()),
        Some(_) => Err(Failure::error(format!(
            "synchronization.{plural} must be a list"
        ))),
        None => Ok(node.get(single).into_iter().collect()),
    }
}

pub(crate) fn check(node: &Node) -> Result<()> {
    for mutex in entries(node, "mutexes", "mutex")? {
        if mutex
            .get("name")
            .map(scalar_text)
            .is_none_or(|name| name.is_empty())
        {
            return Err(Failure::error("synchronization mutex name is required"));
        }
    }
    for semaphore in entries(node, "semaphores", "semaphore")? {
        if semaphore.get("database").is_some() {
            return Err(Failure::unsupported(
                "synchronization database semaphores require an Argo controller; use configMapKeyRef",
            ));
        }
        let reference = semaphore.get("configMapKeyRef").ok_or_else(|| {
            Failure::error("synchronization semaphore configMapKeyRef is required")
        })?;
        for key in ["name", "key"] {
            if reference
                .get(key)
                .map(scalar_text)
                .is_none_or(|value| value.is_empty())
            {
                return Err(Failure::error(format!(
                    "synchronization configMapKeyRef.{key} is required"
                )));
            }
        }
    }
    Ok(())
}

fn limits(
    node: &Node,
    library: &Library,
    near: &Path,
    namespace: &str,
) -> Result<Vec<(String, usize)>> {
    check(node)?;
    let mut limits = Vec::new();
    let namespace_of = |node: &Node| {
        node.get("namespace")
            .map(scalar_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| namespace.into())
    };
    for mutex in entries(node, "mutexes", "mutex")? {
        // Argo keeps database mutexes apart from the others of the same name. One uwe
        // process holds all locks of its runs, as one controller would.
        let database = (mutex.get("database")).is_some_and(|value| value.value == Yaml::Bool(true));
        let kind = if database { "database" } else { "mutex" };
        limits.push((
            format!(
                "{}/{kind}/{}",
                namespace_of(mutex),
                scalar_text(mutex.get("name").unwrap())
            ),
            1,
        ));
    }
    for semaphore in entries(node, "semaphores", "semaphore")? {
        let reference = semaphore.get("configMapKeyRef").unwrap();
        let name = scalar_text(reference.get("name").unwrap());
        let key = scalar_text(reference.get("key").unwrap());
        let namespace = namespace_of(semaphore);
        let inline = semaphore
            .get("limit")
            .or_else(|| reference.get("limit"))
            .map(scalar_text);
        let value = inline.or_else(|| {
            library.all(Kind::ConfigMap, near).into_iter().find(|document| document.name == name && document.node.get("metadata").and_then(|node| node.get("namespace")).map(scalar_text).filter(|value| !value.is_empty()).unwrap_or_else(|| "default".into()) == namespace)
                .and_then(|document| document.node.get("data").and_then(|data| data.get(&key)).map(scalar_text))
        }).ok_or_else(|| Failure::unsupported(format!("synchronization semaphore requires ConfigMap {namespace}/{name} key {key}; supply a ConfigMap with data.{key} using -config, or an inline configMapKeyRef.limit")))?;
        let limit = value.parse::<usize>().ok().filter(|value| *value > 0).ok_or_else(|| Failure::error(format!("synchronization {namespace}/{name}/{key}: semaphore limit must be a positive integer")))?;
        limits.push((format!("{namespace}/semaphore/{name}/{key}"), limit));
    }
    let mut seen = HashSet::new();
    if limits.iter().any(|(key, _)| !seen.insert(key)) {
        return Err(Failure::error("synchronization contains a duplicate lock"));
    }
    Ok(limits)
}

impl Run {
    pub fn lock_workflow(&mut self) -> Result<bool> {
        let Some(mut node) = self.spec.synchronization.clone() else {
            return Ok(true);
        };
        replace_node(&mut node, &Variables::new(self.globals.clone()), false)
            .map_err(Failure::error)?;
        let limits = limits(
            &node,
            &self.library,
            &self.document.path,
            &self.globals["workflow.namespace"],
        )?;
        self.locks
            .borrow_mut()
            .acquire(&format!("workflow:{}", self.workflow_name), &limits)
    }

    pub fn lock_template(&mut self, index: usize, processed: &Processed) -> Result<bool> {
        let Some(node) = &processed.template.synchronization else {
            return Ok(true);
        };
        let limits = limits(
            node,
            &self.library,
            &processed.context.path,
            &self.globals["workflow.namespace"],
        )?;
        let owner = self.nodes[index].id.clone();
        let acquired = self.locks.borrow_mut().acquire(&owner, &limits)?;
        if !acquired {
            self.set_phase(
                index,
                Phase::Pending,
                "Waiting for synchronization lock".into(),
            );
        } else if self.nodes[index].phase == Phase::Pending {
            self.set_phase(index, Phase::Running, String::new());
        }
        Ok(acquired)
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
