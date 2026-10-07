//! Resolves complete pip dependency closures with bounded native backtracking.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use pep440_rs::Version;
use pep508_rs::{ExtraName, Requirement, VersionOrUrl};
use serde::Deserialize;

use crate::distribution::{Candidate, Metadata};
use crate::pypi::{Target, requirement, unsupported};
use crate::{Lock, PipLock};

#[derive(Debug, thiserror::Error)]
pub enum PipError {
    #[error("unsupported pip dependency: {0}")]
    Unsupported(String),
    #[error("pip resolution: {0}")]
    Resolution(String),
    #[error("pip metadata: {0}")]
    Metadata(String),
    #[error("pip acquisition: {0}")]
    Io(#[from] io::Error),
}

#[derive(Deserialize)]
struct Project {
    files: Vec<crate::distribution::Artifact>,
}

#[derive(Clone)]
struct Selected {
    candidate: Candidate,
    metadata: Metadata,
}

struct Resolver<'a, F> {
    lock: &'a Lock,
    target: Target,
    fetch: F,
    candidates: BTreeMap<String, Vec<Candidate>>,
    metadata: BTreeMap<String, Metadata>,
    attempts: usize,
}

/// Resolves against PyPI's JSON Simple API through caller transport, never a host program.
/// The transport must enforce status, size and deadline limits. Tests supply a local index.
pub fn lock_pip(
    lock: &mut Lock,
    fetch: impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> Result<(), PipError> {
    if lock.pip.is_empty() {
        return Ok(());
    }
    validate_pip(&lock.pip)?;
    let target = Target::new(lock)?;
    let roots: Vec<_> = lock
        .pip
        .iter()
        .map(|text| requirement(text))
        .collect::<Result<_, _>>()?;
    let python = target.python.to_string();
    let mut resolver = Resolver {
        lock,
        target,
        fetch,
        candidates: BTreeMap::new(),
        metadata: BTreeMap::new(),
        attempts: 0,
    };
    let selected = resolver.search(&roots, BTreeMap::new())?.ok_or_else(|| {
        PipError::Resolution("requirements have no compatible complete solution".into())
    })?;
    let packages = selected
        .into_values()
        .map(|selected| selected.candidate.package)
        .collect();
    lock.pypi = Some(PipLock {
        format: 1,
        python,
        packages,
    });
    Ok(())
}

/// Rejects unsupported input before Conda metadata or any network is accessed.
pub fn validate_pip(requirements: &[String]) -> Result<(), PipError> {
    for text in requirements {
        requirement(text)?;
    }
    Ok(())
}

impl<F: FnMut(&str) -> io::Result<Vec<u8>>> Resolver<'_, F> {
    fn search(
        &mut self,
        roots: &[Requirement],
        selected: BTreeMap<String, Selected>,
    ) -> Result<Option<BTreeMap<String, Selected>>, PipError> {
        self.attempts += 1;
        if self.attempts > 10_000 || selected.len() > 256 {
            return Err(unsupported(
                "pip resolution exceeds 10000 attempts or 256 projects",
            ));
        }
        let mut required: BTreeMap<String, Vec<Requirement>> = BTreeMap::new();
        for root in roots {
            if root.marker.evaluate(&self.target.markers, &[]) {
                required
                    .entry(root.name.to_string())
                    .or_default()
                    .push(root.clone());
            }
        }
        let mut expanded = BTreeSet::new();
        loop {
            let mut changed = false;
            for (name, package) in &selected {
                let Some(constraints) = required.get(name) else {
                    continue;
                };
                if !constraints
                    .iter()
                    .all(|constraint| matches_version(constraint, &package.candidate.version))
                {
                    return Ok(None);
                }
                let extras: Vec<ExtraName> = constraints
                    .iter()
                    .flat_map(|item| item.extras.iter().cloned())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if extras
                    .iter()
                    .any(|extra| !package.metadata.extras.contains(extra))
                {
                    return Ok(None);
                }
                if !expanded.insert((name.clone(), extras.clone())) {
                    continue;
                }
                changed = true;
                for dependency in &package.metadata.requires {
                    if dependency.marker.evaluate(&self.target.markers, &[])
                        || extras.iter().any(|extra| {
                            dependency
                                .marker
                                .evaluate(&self.target.markers, std::slice::from_ref(extra))
                        })
                    {
                        required
                            .entry(dependency.name.to_string())
                            .or_default()
                            .push(dependency.clone());
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let unresolved = required
            .iter()
            .find(|(name, _)| !selected.contains_key(*name));
        let Some((name, constraints)) = unresolved else {
            return Ok(Some(selected));
        };
        let candidates = self.candidates(name)?.clone();
        let allow_pre = constraints
            .iter()
            .any(|constraint| match &constraint.version_or_url {
                Some(VersionOrUrl::VersionSpecifier(specifiers)) => specifiers
                    .iter()
                    .any(|specifier| specifier.version().is_pre()),
                _ => false,
            });
        for candidate in candidates {
            if !constraints
                .iter()
                .all(|constraint| matches_version(constraint, &candidate.version))
                || (candidate.version.is_pre() && !allow_pre)
            {
                continue;
            }
            let metadata = if let Some(metadata) = self.metadata.get(&candidate.package.url) {
                metadata.clone()
            } else {
                let metadata =
                    crate::distribution::metadata(&candidate, self.lock, &mut self.fetch)?;
                self.metadata
                    .insert(candidate.package.url.clone(), metadata.clone());
                metadata
            };
            if metadata
                .python
                .as_ref()
                .is_some_and(|specifiers| !specifiers.contains(&self.target.python))
            {
                continue;
            }
            let mut branch = selected.clone();
            branch.insert(
                name.clone(),
                Selected {
                    candidate,
                    metadata,
                },
            );
            if let Some(solution) = self.search(roots, branch)? {
                return Ok(Some(solution));
            }
        }
        Ok(None)
    }

    fn candidates(&mut self, name: &str) -> Result<&Vec<Candidate>, PipError> {
        if !self.candidates.contains_key(name) {
            let bytes = (self.fetch)(&format!("https://pypi.org/simple/{name}/"))?;
            let project: Project = serde_json::from_slice(&bytes)
                .map_err(|error| PipError::Metadata(error.to_string()))?;
            let mut candidates = Vec::new();
            for artifact in project.files {
                if let Some(candidate) = artifact.candidate(name, &self.target)? {
                    candidates.push(candidate);
                }
            }
            candidates.sort_by(|a, b| {
                b.version
                    .cmp(&a.version)
                    .then_with(|| {
                        b.package
                            .file
                            .ends_with(".whl")
                            .cmp(&a.package.file.ends_with(".whl"))
                    })
                    .then_with(|| a.package.file.cmp(&b.package.file))
            });
            if candidates.is_empty() {
                return Err(unsupported(format!(
                    "{name} has no hashed artifact compatible with locked Python and target platform"
                )));
            }
            self.candidates.insert(name.to_owned(), candidates);
        }
        Ok(self.candidates.get(name).expect("loaded candidates"))
    }
}

fn matches_version(requirement: &Requirement, version: &Version) -> bool {
    match &requirement.version_or_url {
        Some(VersionOrUrl::VersionSpecifier(specifiers)) => specifiers.contains(version),
        None => true,
        Some(VersionOrUrl::Url(_)) => false,
    }
}
