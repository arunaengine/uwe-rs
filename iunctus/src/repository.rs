//! Resolves hosted pipeline revisions and caches bounded repository archives.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use nextflow_parser::ast::{ConfigStatement, ExpressionKind, Literal};
use reqwest::{Response, Url};
use workflow_graph::DriverError;

use crate::{Failure, failure, revision};

pub(crate) const MARKER: &str = ".iunctus-revision";
const MOST_BYTES: u64 = 256 << 20;
const MOST_FILES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Host {
    Github,
    Gitlab,
    Bitbucket,
    Gitea,
}

pub(crate) struct Repository {
    host: Host,
    origin: String,
    owner: String,
    name: String,
}

impl Repository {
    pub(crate) fn parse(text: &str) -> Result<Option<Self>, Failure> {
        let address = if text.contains("://") {
            text.to_owned()
        } else {
            let parts: Vec<_> = text.split('/').collect();
            if parts.len() != 2 || !parts.iter().all(|part| valid_name(part)) {
                return Ok(None);
            }
            format!("https://github.com/{text}")
        };
        let url = Url::parse(&address).map_err(|_| Failure::usage("invalid repository URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Ok(None);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Failure::usage(
                "repository URL credentials are refused; use a token environment variable",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Ok(None);
        }
        let host = match url.host_str().unwrap_or_default() {
            "github.com" => Host::Github,
            "gitlab.com" => Host::Gitlab,
            "bitbucket.org" => Host::Bitbucket,
            name if name.split('.').any(|part| part == "gitlab") => Host::Gitlab,
            _ => Host::Gitea,
        };
        let path = url.path().trim_matches('/').trim_end_matches(".git");
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() < 2
            || (host != Host::Gitlab && parts.len() != 2)
            || !parts.iter().all(|part| valid_name(part))
            || parts.last().is_some_and(|name| {
                [".cwl", ".yaml", ".yml", ".json", ".nf", ".smk"]
                    .iter()
                    .any(|end| name.ends_with(end))
            })
        {
            return Ok(None);
        }
        Ok(Some(Self {
            host,
            origin: url.origin().ascii_serialization(),
            owner: parts[..parts.len() - 1].join("/"),
            name: parts[parts.len() - 1].into(),
        }))
    }

    fn folder(&self, cache: &Path) -> PathBuf {
        let host = self.origin.split_once("://").unwrap().1;
        cache
            .join("pipelines")
            .join(encode(host))
            .join(&self.owner)
            .join(&self.name)
    }

    fn api(&self) -> String {
        let path = format!("{}/{}", self.owner, self.name);
        match self.host {
            Host::Github => format!("https://api.github.com/repos/{path}"),
            Host::Gitlab => format!("{}/api/v4/projects/{}", self.origin, encode(&path)),
            Host::Bitbucket => format!("https://api.bitbucket.org/2.0/repositories/{path}"),
            Host::Gitea => format!("{}/api/v1/repos/{path}", self.origin),
        }
    }

    fn token(&self) -> Option<String> {
        let names: &[&str] = match self.host {
            Host::Github => &["NXF_GITHUB_ACCESS_TOKEN", "GITHUB_TOKEN"],
            Host::Gitlab => &["GITLAB_TOKEN"],
            Host::Bitbucket => &["BITBUCKET_TOKEN"],
            Host::Gitea => &["GITEA_TOKEN"],
        };
        names
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|text| !text.is_empty()))
    }

    async fn request(&self, url: &str, token: Option<&str>) -> Result<Response, Failure> {
        let client = storage::http_client().map_err(Failure::setup)?;
        let mut request = client.get(url).timeout(Duration::from_secs(120));
        if let Some(token) = token {
            let mut header = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| {
                    Failure::setup("invalid repository token; check its environment variable")
                })?;
            header.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        let response = request.send().await.map_err(|_| failure("repository network request failed; retry online, or use a cached full commit id with -r"))?;
        if !response.status().is_success() {
            return Err(failure(format!(
                "repository server returned HTTP {}; check the repository, revision and token",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }

    async fn json(&self, url: &str, token: Option<&str>) -> Result<serde_json::Value, Failure> {
        let mut response = self.request(url, token).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure("repository response interrupted; retry online"))?
        {
            if bytes.len() + chunk.len() > 4 << 20 {
                return Err(failure("repository API response is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid repository API response"))
    }

    async fn resolve(&self, reference: &str, token: Option<&str>) -> Result<String, Failure> {
        let reference = encode(reference);
        let suffix = match self.host {
            Host::Gitlab => "repository/commits",
            Host::Bitbucket => "commit",
            _ => "commits",
        };
        let value = self
            .json(&format!("{}/{suffix}/{reference}", self.api()), token)
            .await?;
        let key = match self.host {
            Host::Gitlab => "id",
            Host::Bitbucket => "hash",
            _ => "sha",
        };
        value
            .get(key)
            .and_then(|value| value.as_str())
            .and_then(revision::commit)
            .ok_or_else(|| failure("repository API returned no full commit id"))
    }

    async fn download(
        &self,
        commit: &str,
        parent: &Path,
        token: Option<&str>,
    ) -> Result<PathBuf, Failure> {
        let folder = parent.join(commit);
        if ready(&folder, commit) {
            return Ok(folder);
        }
        std::fs::create_dir_all(parent).map_err(local)?;
        let temporary = tempfile::tempdir_in(parent).map_err(local)?;
        let url = match self.host {
            Host::Github => format!("{}/tarball/{commit}", self.api()),
            Host::Gitlab => format!("{}/repository/archive.tar.gz?sha={commit}", self.api()),
            Host::Bitbucket => format!(
                "{}/{}/{}/get/{commit}.tar.gz",
                self.origin, self.owner, self.name
            ),
            Host::Gitea => format!("{}/archive/{commit}.tar.gz", self.api()),
        };
        let mut response = self.request(&url, token).await?;
        let mut archive = tempfile::tempfile().map_err(local)?;
        let mut size = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure("repository archive interrupted; retry online"))?
        {
            size += chunk.len() as u64;
            if size > MOST_BYTES {
                return Err(failure("repository archive exceeds 256 MB"));
            }
            archive.write_all(&chunk).map_err(local)?;
        }
        use std::io::{Seek, SeekFrom};
        archive.seek(SeekFrom::Start(0)).map_err(local)?;
        unpack(archive, temporary.path())?;
        std::fs::write(temporary.path().join(MARKER), commit).map_err(local)?;
        // Another process may have published this immutable commit while we downloaded it.
        if let Err(error) = std::fs::rename(temporary.path(), &folder)
            && !ready(&folder, commit)
        {
            return Err(local(error));
        }
        Ok(folder)
    }

    pub(crate) async fn fetch(
        &self,
        cache: &Path,
        reference: Option<&str>,
        latest: bool,
        token: Option<&str>,
    ) -> Result<PathBuf, Failure> {
        let parent = self.folder(cache);
        if let Some(commit) = reference
            .and_then(revision::commit)
            .map(|text| text.to_ascii_lowercase())
        {
            let folder = parent.join(&commit);
            if !latest && ready(&folder, &commit) {
                return Ok(folder);
            }
            return match self.resolve(&commit, token).await {
                Ok(found) if found == commit => self.download(&commit, &parent, token).await,
                Ok(_) => Err(failure(
                    "repository resolved a different commit than requested",
                )),
                Err(_) if ready(&folder, &commit) => Ok(folder),
                Err(error) => Err(error),
            };
        }
        let references = parent.join("references.json");
        let mut remembered: BTreeMap<String, String> = std::fs::read(&references)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let branch = match reference {
            Some(reference) => reference.to_owned(),
            None => {
                let value = self.json(&self.api(), token).await?;
                value
                    .get("default_branch")
                    .or_else(|| {
                        value
                            .get("mainbranch")
                            .and_then(|branch| branch.get("name"))
                    })
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| failure("repository has no default branch"))?
                    .to_owned()
            }
        };
        // Symbolic requests must reach the server even when their checkout is cached.
        let resolved = self.resolve(&branch, token).await?;
        let key = reference.unwrap_or("");
        let cached = remembered
            .get(key)
            .filter(|commit| !latest && ready(&parent.join(commit), commit))
            .cloned();
        let commit = cached.clone().unwrap_or(resolved);
        let mut folder = self.download(&commit, &parent, token).await?;
        let mut commit = commit;
        if reference.is_none()
            && cached.is_none()
            && let Some(default) = default_branch(&folder)?
            && default != branch
        {
            commit = self.resolve(&default, token).await?;
            folder = self.download(&commit, &parent, token).await?;
        }
        remembered.insert(key.into(), commit);
        let mut saved = tempfile::NamedTempFile::new_in(&parent).map_err(local)?;
        serde_json::to_writer(saved.as_file_mut(), &remembered)
            .map_err(|_| failure("cannot record repository revision"))?;
        saved
            .persist(&references)
            .map_err(|error| local(error.error))?;
        Ok(folder)
    }
}

pub(crate) fn root(path: &Path) -> Option<(PathBuf, String)> {
    let path = std::path::absolute(path).ok()?;
    path.ancestors().find_map(|folder| {
        let text = std::fs::read_to_string(folder.join(MARKER)).ok()?;
        Some((folder.to_owned(), revision::commit(text.trim())?))
    })
}

pub(crate) fn prepare(
    target: &Path,
    reference: Option<&str>,
    latest: bool,
) -> Result<PathBuf, Failure> {
    if target.exists() {
        if reference.is_some() || latest {
            return Err(Failure::usage(
                "-r and -latest require a repository name or URL; use the local workflow without revision options",
            ));
        }
        return Ok(target.to_owned());
    }
    let Some(repository) = Repository::parse(&target.to_string_lossy())? else {
        return Ok(target.to_owned());
    };
    let runtime = workflow_graph::runtime().map_err(|error| Failure::setup(error.to_string()))?;
    runtime.block_on(repository.fetch(
        &crate::cache_directory(),
        reference,
        latest,
        repository.token().as_deref(),
    ))
}

fn ready(folder: &Path, commit: &str) -> bool {
    revision::commit(commit).is_some()
        && std::fs::read_to_string(folder.join(MARKER)).is_ok_and(|text| text.trim() == commit)
}

fn valid_name(text: &str) -> bool {
    !text.is_empty()
        && !matches!(text, "." | "..")
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn local(error: std::io::Error) -> Failure {
    Failure::setup(format!(
        "repository cache: {error}; check the cache directory"
    ))
}

fn unpack(reader: impl Read, folder: &Path) -> Result<(), Failure> {
    let reader = flate2::read::GzDecoder::new(reader).take(MOST_BYTES + 1);
    let mut archive = tar::Archive::new(reader);
    let mut size = 0;
    let mut top = None;
    for (count, entry) in archive.entries().map_err(local)?.enumerate() {
        let mut entry = entry.map_err(local)?;
        if count >= MOST_FILES {
            return Err(failure("oversized repository archive"));
        }
        if entry.header().entry_type().is_pax_global_extensions() {
            continue;
        }
        let path = entry.path().map_err(local)?.into_owned();
        let parts: Vec<_> = path.components().collect();
        if parts
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(failure("unsafe or oversized repository archive"));
        }
        let Some(Component::Normal(first)) = parts.first() else {
            continue;
        };
        if top.as_ref().is_some_and(|top| top != first) {
            return Err(failure("repository archive has multiple roots"));
        }
        top = Some(first.to_os_string());
        let relative: PathBuf = parts.iter().skip(1).collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if parts
            .iter()
            .skip(1)
            .any(|part| part.as_os_str() == ".git" || part.as_os_str() == MARKER)
        {
            return Err(failure(
                "repository archive contains reserved cache metadata",
            ));
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(DriverError::unsupported("repository archive links are unsupported; use a local workflow with materialized files").into());
        }
        size += entry.size();
        if size > MOST_BYTES {
            return Err(failure("repository contents exceed 256 MB"));
        }
        let target = folder.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(local)?;
        }
        entry.unpack(target).map_err(local)?;
    }
    if std::fs::read_to_string(folder.join(".gitmodules")).is_ok_and(|text| {
        text.lines()
            .any(|line| line.trim_start().starts_with("[submodule"))
    }) {
        return Err(DriverError::unsupported("repository submodules are unsupported; use a local workflow with materialized submodule files").into());
    }
    Ok(())
}

fn default_branch(folder: &Path) -> Result<Option<String>, Failure> {
    let Ok(text) = std::fs::read_to_string(folder.join("nextflow.config")) else {
        return Ok(None);
    };
    let parsed = nextflow_parser::parse_config(&text);
    if !parsed.errors.is_empty() {
        return Err(failure("cannot parse repository nextflow.config"));
    }
    fn find(statements: &[ConfigStatement], manifest: bool) -> Result<Option<String>, Failure> {
        let mut found = None;
        for statement in statements {
            match statement {
                ConfigStatement::Block { name, body, .. }
                    if !manifest && name.text == "manifest" =>
                {
                    if let Some(branch) = find(body, true)? {
                        found = Some(branch);
                    }
                }
                ConfigStatement::Assign { path, value, .. }
                    if (manifest && path.len() == 1 && path[0].text == "defaultBranch")
                        || (!manifest
                            && path.len() == 2
                            && path[0].text == "manifest"
                            && path[1].text == "defaultBranch") =>
                {
                    found = match &value.kind {
                        ExpressionKind::Literal(Literal::String(branch)) => Some(branch.clone()),
                        ExpressionKind::Literal(Literal::Null) => None,
                        _ => return Err(DriverError::unsupported("manifest.defaultBranch must be a literal string for repository runs; choose a revision with -r").into()),
                    };
                }
                _ => {}
            }
        }
        Ok(found)
    }
    find(&parsed.tree.statements, false)
}

#[cfg(test)]
#[path = "repository_tests.rs"]
mod tests;
