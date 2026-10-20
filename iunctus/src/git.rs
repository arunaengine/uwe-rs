//! Fetches commits of Git repositories over HTTPS with gix and writes their files out.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use gix::remote::Direction;

/// Submodules nest at most this deep, which stops loops of repositories.
const NESTING: usize = 8;

/// What to fetch: a repository, a branch, tag or commit of it (its default branch without
/// one), how much history, and the account for a server that asks for one.
#[derive(Clone, Debug, Default)]
pub struct Fetch {
    pub url: String,
    pub reference: Option<String>,
    /// Commits of history to fetch; all with `None`, as a shallow fetch cannot reach any
    /// commit by its ID.
    pub depth: Option<u32>,
    /// More refspecs to fetch, like `refs/pull/1/head:refs/pull/1/head`.
    pub refspecs: Vec<String>,
    pub submodules: bool,
    pub account: Option<(String, String)>,
}

/// The commit that a reference names on the server, without fetching any objects.
pub fn resolve(fetch: &Fetch) -> Result<String, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    let repository = bare(folder.path())?;
    let remote = remote(&repository, fetch)?;
    let connection = remote.connect(Direction::Fetch).map_err(failed(fetch))?;
    let connection = with_account(connection, fetch);
    let prepared = (connection.prepare_fetch(gix::progress::Discard, Default::default()))
        .map_err(failed(fetch))?;
    commit_of(fetch, prepared.ref_map())
}

/// Fetches the commit that the reference names and writes its files into `target`, which
/// must not exist yet. Gives the full commit ID.
pub fn checkout(fetch: &Fetch, target: &Path, stop: &AtomicBool) -> Result<String, String> {
    checkout_nested(fetch, target, stop, 0)
}

fn checkout_nested(
    fetch: &Fetch,
    target: &Path,
    stop: &AtomicBool,
    depth: usize,
) -> Result<String, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    let repository = bare(folder.path())?;
    let remote = remote(&repository, fetch)?;
    let connection = remote.connect(Direction::Fetch).map_err(failed(fetch))?;
    let connection = with_account(connection, fetch);
    let mut prepared = (connection.prepare_fetch(gix::progress::Discard, Default::default()))
        .map_err(failed(fetch))?;
    let commit = commit_of(fetch, prepared.ref_map())?;
    if let Some(depth) = fetch.depth.and_then(NonZeroU32::new) {
        prepared = prepared.with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(depth));
    }
    prepared
        .receive(gix::progress::Discard, stop)
        .map_err(failed(fetch))?;
    let id = gix::ObjectId::from_hex(commit.as_bytes()).map_err(|error| error.to_string())?;
    let tree = (repository.find_commit(id))
        .and_then(|commit| commit.tree_id())
        .map_err(|_| format!("{} does not hold commit {commit}", fetch.url))?;
    std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
    let mut links = Vec::new();
    write_tree(
        &repository,
        tree.detach(),
        target,
        Path::new(""),
        &mut links,
    )?;
    if fetch.submodules && !links.is_empty() {
        if depth >= NESTING {
            return Err(format!("submodules of {} nest too deep", fetch.url));
        }
        let modules = std::fs::read(target.join(".gitmodules")).unwrap_or_default();
        for (path, commit) in links {
            let url = submodule_url(&modules, &path, &fetch.url)
                .ok_or_else(|| format!("{} names no URL for submodule {path}", fetch.url))?;
            let module = Fetch {
                url,
                reference: Some(commit),
                depth: None,
                refspecs: Vec::new(),
                submodules: true,
                account: fetch.account.clone(),
            };
            let place = target.join(&path);
            let _ = std::fs::remove_dir(&place);
            checkout_nested(&module, &place, stop, depth + 1)?;
        }
    }
    Ok(commit)
}

/// An empty bare repository that reads no configuration of this machine or the user, so that
/// no credential helper or other program of theirs runs.
fn bare(folder: &Path) -> Result<gix::Repository, String> {
    let options = gix::open::Options::isolated();
    let kind = gix::create::Kind::Bare;
    gix::ThreadSafeRepository::init_opts(folder, kind, Default::default(), options)
        .map(Into::into)
        .map_err(|error| error.to_string())
}

fn remote<'a>(repository: &'a gix::Repository, fetch: &Fetch) -> Result<gix::Remote<'a>, String> {
    if !fetch.url.starts_with("https://") && !fetch.url.starts_with("http://") {
        return Err(format!(
            "{}: only Git over HTTP and HTTPS is supported",
            fetch.url
        ));
    }
    let mut specs = match fetch.reference.as_deref() {
        None => vec!["+HEAD:refs/remotes/origin/HEAD".to_owned()],
        Some(commit) if is_commit(commit) => vec![
            "+refs/heads/*:refs/remotes/origin/*".to_owned(),
            "+refs/tags/*:refs/tags/*".to_owned(),
        ],
        Some(name) if name.starts_with("refs/") => vec![format!("+{name}:{name}")],
        Some(name) => vec![
            format!("+refs/heads/{name}:refs/remotes/origin/{name}"),
            format!("+refs/tags/{name}:refs/tags/{name}"),
        ],
    };
    specs.extend(fetch.refspecs.iter().cloned());
    let remote = repository
        .remote_at(fetch.url.as_str())
        .map_err(|error| error.to_string())?;
    remote
        .with_refspecs(specs.iter().map(String::as_str), Direction::Fetch)
        .map_err(|error| error.to_string())
}

fn with_account<'a, T>(
    connection: gix::remote::Connection<'a, 'static, 'a, T>,
    fetch: &Fetch,
) -> gix::remote::Connection<'a, 'static, 'a, T>
where
    T: gix::protocol::transport::client::blocking_io::Transport,
{
    let account = fetch.account.clone();
    // Only the account of the settings, never a helper program or a prompt.
    connection.with_credentials(move |action| {
        let gix::credentials::helper::Action::Get(context) = action else {
            return Ok(None);
        };
        Ok(account
            .clone()
            .map(|(username, password)| gix::credentials::protocol::Outcome {
                identity: gix::sec::identity::Account {
                    username,
                    password,
                    oauth_refresh_token: None,
                },
                next: context.into(),
            }))
    })
}

fn failed(fetch: &Fetch) -> impl Fn(gix::Error) -> String + '_ {
    move |error| format!("cannot fetch {}: {error}", fetch.url)
}

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The commit of the reference among the refs that the server listed.
fn commit_of(fetch: &Fetch, map: &gix::remote::fetch::RefMap) -> Result<String, String> {
    let wanted = match fetch.reference.as_deref() {
        Some(commit) if is_commit(commit) => return Ok(commit.to_ascii_lowercase()),
        Some(name) if name.starts_with("refs/") => vec![name.to_owned()],
        Some(name) => vec![format!("refs/heads/{name}"), format!("refs/tags/{name}")],
        None => vec!["HEAD".to_owned()],
    };
    for name in &wanted {
        for reference in &map.remote_refs {
            let (full, target, peeled) = reference.unpack();
            if full == name.as_bytes().as_bstr()
                && let Some(id) = peeled.or(target)
            {
                return Ok(id.to_hex().to_string());
            }
        }
    }
    let shown = fetch.reference.as_deref().unwrap_or("HEAD");
    Err(format!(
        "{} has no branch, tag or commit {shown}",
        fetch.url
    ))
}

/// Writes the files of a tree, as a checkout does; submodules are noted for later.
fn write_tree(
    repository: &gix::Repository,
    tree: gix::ObjectId,
    target: &Path,
    inside: &Path,
    links: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let failed = |error: std::io::Error| error.to_string();
    let tree = repository
        .find_tree(tree)
        .map_err(|error| error.to_string())?;
    for entry in tree.iter() {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .filename()
            .to_str()
            .map_err(|_| "a file name is no UTF-8")?;
        if matches!(name, "" | "." | ".." | ".git") || name.contains('/') {
            return Err(format!(
                "the repository holds an invalid file name {name:?}"
            ));
        }
        let relative = inside.join(name);
        let path = target.join(&relative);
        let id = entry.oid().to_owned();
        match entry.mode().kind() {
            EntryKind::Tree => {
                std::fs::create_dir_all(&path).map_err(failed)?;
                write_tree(repository, id, target, &relative, links)?;
            }
            EntryKind::Commit => {
                std::fs::create_dir_all(&path).map_err(failed)?;
                links.push((
                    relative.to_string_lossy().into_owned(),
                    id.to_hex().to_string(),
                ));
            }
            kind => {
                let blob = repository
                    .find_blob(id)
                    .map_err(|error| error.to_string())?;
                if kind == EntryKind::Link {
                    let link = blob.data.to_str().map_err(|_| "a link is no UTF-8")?;
                    std::os::unix::fs::symlink(link, &path).map_err(failed)?;
                    continue;
                }
                std::fs::write(&path, &blob.data).map_err(failed)?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::Permissions::from_mode(0o755);
                    std::fs::set_permissions(&path, mode).map_err(failed)?;
                }
            }
        }
    }
    Ok(())
}

/// The URL of the submodule at `path` in a `.gitmodules` file; relative URLs start at the
/// URL of the repository that holds it, as Git resolves them.
fn submodule_url(modules: &[u8], path: &str, parent: &str) -> Option<String> {
    let text = std::str::from_utf8(modules).ok()?;
    let mut section_path = None;
    let mut section_url = None;
    let mut found = None;
    for line in text.lines().map(str::trim).chain(["[end]"]) {
        if line.starts_with('[') {
            if section_path.as_deref() == Some(path) {
                found = section_url.take();
                break;
            }
            (section_path, section_url) = (None, None);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "path" => section_path = Some(value),
            "url" => section_url = Some(value),
            _ => {}
        }
    }
    let url: String = found?;
    if !url.starts_with("../") && !url.starts_with("./") {
        return Some(url);
    }
    let mut base = parent
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_owned();
    let mut rest = url.as_str();
    loop {
        if let Some(after) = rest.strip_prefix("../") {
            base = base.rsplit_once('/')?.0.to_owned();
            rest = after;
        } else if let Some(after) = rest.strip_prefix("./") {
            rest = after;
        } else {
            break;
        }
    }
    Some(format!("{base}/{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_submodule_urls() {
        let modules = b"[submodule \"lib\"]\n\tpath = lib\n\turl = ../tools.git\n\
                        [submodule \"data\"]\n\tpath = data\n\turl = https://example.org/data.git\n";
        let parent = "https://git.example.org/team/pipeline.git";
        assert_eq!(
            submodule_url(modules, "lib", parent).as_deref(),
            Some("https://git.example.org/team/tools.git")
        );
        assert_eq!(
            submodule_url(modules, "data", parent).as_deref(),
            Some("https://example.org/data.git")
        );
        assert_eq!(submodule_url(modules, "other", parent), None);
    }
}
