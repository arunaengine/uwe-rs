//! Reads Argo git artifacts into checkouts that the Git client of the run makes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Arc;

use argo_parser::{Node, Value as Yaml};

use crate::run::{Failure, Result, scalar_text};

/// Fields of a git artifact that a checkout over HTTPS honours.
const FIELDS: [&str; 9] = [
    "repo",
    "revision",
    "depth",
    "fetch",
    "branch",
    "singleBranch",
    "disableSubmodules",
    "usernameSecret",
    "passwordSecret",
];

/// A Git checkout that an artifact asks for: the repository, a commit, branch or tag, how
/// much history, more refspecs, submodules, and whether it signs in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitSource {
    pub repo: String,
    pub reference: Option<String>,
    pub depth: Option<u32>,
    pub fetch: Vec<String>,
    pub submodules: bool,
    /// Whether the artifact names a user and a password; uwe takes the account that its
    /// config file gives the address instead of Kubernetes secrets.
    pub account: bool,
}

/// Checks a Git source out into a folder that does not exist yet.
pub type Checkout = dyn Fn(&GitSource, &Path) -> std::result::Result<(), String> + Send + Sync;

/// The Git client of a run.
#[derive(Clone)]
pub struct GitClient(pub Arc<Checkout>);

impl std::fmt::Debug for GitClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GitClient")
    }
}

/// Refuses what a checkout over HTTPS cannot honour: SSH and fields of other kinds.
pub(crate) fn check(artifact: &str, git: &Node) -> Result<()> {
    let refused = |message: String| {
        Err(Failure::unsupported(format!(
            "artifact {artifact}: {message}"
        )))
    };
    let repo = git.get("repo").map(scalar_text).unwrap_or_default();
    if !crate::tags::has_tag(&repo) && !repo.starts_with("https://") && !repo.starts_with("http://")
    {
        return refused(format!(
            "git repository {repo} is no HTTPS address; uwe fetches Git over HTTPS only"
        ));
    }
    if let Yaml::Mapping(fields) = &git.value {
        for field in fields
            .iter()
            .filter(|field| field.value.value != Yaml::Null)
        {
            let key = field.key.as_str();
            let off = matches!(field.value.value, Yaml::Bool(false));
            if !FIELDS.contains(&key) && !(off && key == "insecureIgnoreHostKey") {
                return refused(format!(
                    "git.{key} is not supported; it belongs to Git over SSH or needs a secret store"
                ));
            }
        }
    }
    Ok(())
}

/// The checkout that a git artifact asks for, as Argo's git driver makes it.
pub(crate) fn source(git: &Node) -> GitSource {
    let text = |key: &str| {
        git.get(key)
            .map(scalar_text)
            .filter(|text| !text.is_empty())
    };
    let yes = |key: &str| {
        git.get(key)
            .is_some_and(|node| node.value == Yaml::Bool(true))
    };
    let fetch = match git.get("fetch").map(|node| &node.value) {
        Some(Yaml::Sequence(items)) => items.iter().map(scalar_text).collect(),
        _ => Vec::new(),
    };
    GitSource {
        repo: text("repo").unwrap_or_default(),
        // A revision wins over the branch, which Argo checks out first.
        reference: text("revision").or_else(|| text("branch")),
        depth: text("depth")
            .and_then(|depth| depth.parse().ok())
            .filter(|depth| *depth > 0),
        fetch,
        submodules: !yes("disableSubmodules"),
        account: git.get("usernameSecret").is_some() || git.get("passwordSecret").is_some(),
    }
}
