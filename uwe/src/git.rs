//! Fetches commits of Git repositories over HTTPS with gix and writes their files out.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gix::bstr::ByteSlice;
use gix::credentials::protocol::Outcome;
use gix::objs::tree::EntryKind;
use gix::remote::Direction;

/// Submodules nest at most this deep, which stops loops of repositories.
const NESTING: usize = 8;

/// What to fetch: a repository, a branch, tag or commit of it (its default branch without
/// one), how much history, and where the accounts for servers that ask for one come from.
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
    pub accounts: Option<Accounts>,
}

/// The user and password, or token, of a repository URL; each submodule looks up its own.
#[derive(Clone)]
pub struct Accounts(pub Arc<AccountLookup>);

type AccountLookup = dyn Fn(&str) -> Result<Option<(String, String)>, String> + Send + Sync;

impl std::fmt::Debug for Accounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Accounts")
    }
}

/// The commit that a reference names on the server, without fetching any objects.
pub fn resolve(fetch: &Fetch) -> Result<String, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    let repository = bare(folder.path())?;
    let remote = remote(&repository, fetch)?;
    let connection = remote.connect(Direction::Fetch).map_err(failed(fetch))?;
    let connection = connection.with_credentials(answer(&fetch.url, account_of(fetch)?));
    let prepared = (connection.prepare_fetch(gix::progress::Discard, Default::default()))
        .map_err(failed(fetch))?;
    commit_of(fetch, prepared.ref_map())
}

/// Fetches the commit that the reference names and writes its files into `target`, which
/// must not exist yet. Gives the full commit ID.
pub fn checkout(fetch: &Fetch, target: &Path, stop: &AtomicBool) -> Result<String, String> {
    let commit = checkout_nested(fetch, target, stop, 0)?;
    contain_links(target).map_err(|error| error.to_string())?;
    Ok(commit)
}

/// Writes links that lead out of the checkout as plain files that hold their target, as Git
/// does without `core.symlinks`, so that nothing reads through them; later links count too.
fn contain_links(root: &Path) -> std::io::Result<()> {
    let mut links = Vec::new();
    let mut pending = vec![std::path::PathBuf::new()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&folder))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = folder.join(entry.file_name());
            if kind.is_symlink() {
                links.push(path);
            } else if kind.is_dir() {
                pending.push(path);
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for link in &links {
            let place = root.join(link);
            let linked = std::fs::symlink_metadata(&place).is_ok_and(|meta| meta.is_symlink());
            if !linked || task_runner::resolved(root, link).is_some() {
                continue;
            }
            let target = std::fs::read_link(&place)?;
            std::fs::remove_file(&place)?;
            std::fs::write(&place, target.as_os_str().as_encoded_bytes())?;
            changed = true;
        }
    }
    Ok(())
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
    let connection = connection.with_credentials(answer(&fetch.url, account_of(fetch)?));
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
    let mut symlinks = Vec::new();
    write_tree(
        &repository,
        tree.detach(),
        target,
        Path::new(""),
        &mut links,
        &mut symlinks,
    )?;
    if fetch.submodules && !links.is_empty() {
        if depth >= NESTING {
            return Err(format!("submodules of {} nest too deep", fetch.url));
        }
        let modules = std::fs::read(target.join(".gitmodules")).unwrap_or_default();
        for (path, commit) in links {
            real_folder(target, Path::new(&path))?;
            let url = submodule_url(&modules, &path, &fetch.url)
                .ok_or_else(|| format!("{} names no URL for submodule {path}", fetch.url))?;
            let module = Fetch {
                url,
                reference: Some(commit),
                depth: None,
                refspecs: Vec::new(),
                submodules: true,
                accounts: fetch.accounts.clone(),
            };
            let place = target.join(&path);
            let _ = std::fs::remove_dir(&place);
            checkout_nested(&module, &place, stop, depth + 1)?;
        }
    }
    // Links come last, so that no file or submodule above is written through one.
    write_links(target, symlinks)?;
    Ok(commit)
}

/// Makes the noted links, each in a real folder of the checkout and never over a path.
fn write_links(target: &Path, symlinks: Vec<(std::path::PathBuf, String)>) -> Result<(), String> {
    for (relative, link) in symlinks {
        if let Some(parent) = relative.parent() {
            real_folder(target, parent)?;
        }
        std::os::unix::fs::symlink(link, target.join(&relative))
            .map_err(|error| format!("{}: {error}", relative.display()))?;
    }
    Ok(())
}

/// Fails unless each part of `relative` is a real folder in `root`, not a link or a file.
fn real_folder(root: &Path, relative: &Path) -> Result<(), String> {
    let mut place = root.to_path_buf();
    for part in relative.components() {
        place.push(part);
        let meta = std::fs::symlink_metadata(&place).map_err(|error| error.to_string())?;
        if !meta.is_dir() {
            return Err(format!(
                "{} is no folder of the checkout",
                relative.display()
            ));
        }
    }
    Ok(())
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

/// The account for the URL of a fetch, only over HTTPS, so that none goes in clear text.
fn account_of(fetch: &Fetch) -> Result<Option<(String, String)>, String> {
    account(&fetch.url, fetch.accounts.as_ref())
}

/// The account for a URL, only over HTTPS; AWS CodeCommit gets one derived from AWS keys.
pub(crate) fn account(
    url: &str,
    accounts: Option<&Accounts>,
) -> Result<Option<(String, String)>, String> {
    if !url.starts_with("https://") {
        return Ok(None);
    }
    let given = match accounts {
        Some(accounts) => (accounts.0)(url)?,
        None => None,
    };
    if !codecommit(url)? {
        return Ok(given);
    }
    let variable = |name: &str| std::env::var(name).ok();
    codecommit_account(url, given, &variable, jiff::Timestamp::now()).map(Some)
}

/// Whether the URL names an AWS CodeCommit repository, whose host must name its region.
fn codecommit(url: &str) -> Result<bool, String> {
    let host = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = host.split(['/', ':']).next().unwrap_or_default();
    let Some(middle) =
        (host.strip_prefix("git-codecommit.")).and_then(|rest| rest.strip_suffix(".amazonaws.com"))
    else {
        return Ok(false);
    };
    if middle.is_empty()
        || !middle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!("Invalid AWS CodeCommit hostname: '{host}'"));
    }
    Ok(true)
}

/// The CodeCommit account at `time`: a user and password of the settings are the AWS access
/// key and secret, else AWS's default credential chain finds the keys.
fn codecommit_account(
    url: &str,
    given: Option<(String, String)>,
    variable: &dyn Fn(&str) -> Option<String>,
    time: jiff::Timestamp,
) -> Result<(String, String), String> {
    use storage::aws;
    let credentials = match given {
        Some((id, secret)) => aws::Credentials::Keys(aws::Keys {
            id,
            secret,
            token: None,
        }),
        None => aws::Credentials::Chain {
            environment: (aws::VARIABLES.iter())
                .filter_map(|name| Some((name.to_string(), variable(name)?)))
                .collect(),
            home: variable("HOME").map(Into::into),
        },
    };
    let keys = aws::resolve(&credentials)
        .map_err(|error| format!("Unable to retrieve AWS Credentials - {error}"))?;
    aws::git_account(url, &keys, time)
        .map_err(|error| format!("Error calculating AWS CodeCommit password - {error}"))
}

/// Answers the questions of the server for an account: only the account of the settings, never
/// a helper program or a prompt, and only to the scheme and host of `url`, not after a redirect.
fn answer(
    url: &str,
    account: Option<(String, String)>,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::Result<Option<Outcome>> + use<> {
    let expected = origin(gix::credentials::protocol::Context {
        url: Some(url.into()),
        ..Default::default()
    });
    move |action| {
        let gix::credentials::helper::Action::Get(context) = action else {
            return Ok(None);
        };
        let asked = origin(context.clone());
        let allowed = asked.is_some() && asked == expected;
        Ok(account
            .clone()
            .filter(|_| allowed)
            .map(|(username, password)| Outcome {
                identity: gix::sec::identity::Account {
                    username,
                    password,
                    oauth_refresh_token: None,
                },
                next: context.into(),
            }))
    }
}

/// The host of an HTTPS address in a credential question, with its port if not the default.
fn origin(mut context: gix::credentials::protocol::Context) -> Option<String> {
    context.destructure_url_in_place(false).ok()?;
    (context.protocol.as_deref() == Some("https"))
        .then_some(context.host)
        .flatten()
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

/// Writes the files of a tree, as a checkout does; submodules and symbolic links are noted
/// for later. Nothing is written over an existing path, so a name given twice fails.
fn write_tree(
    repository: &gix::Repository,
    tree: gix::ObjectId,
    target: &Path,
    inside: &Path,
    links: &mut Vec<(String, String)>,
    symlinks: &mut Vec<(std::path::PathBuf, String)>,
) -> Result<(), String> {
    let failed = |error: std::io::Error| error.to_string();
    let tree = repository
        .find_tree(tree)
        .map_err(|error| error.to_string())?;
    let mut names = std::collections::HashSet::new();
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
        if !names.insert(name.to_owned()) {
            return Err(format!("the repository holds {} twice", relative.display()));
        }
        let path = target.join(&relative);
        let id = entry.oid().to_owned();
        match entry.mode().kind() {
            EntryKind::Tree => {
                std::fs::create_dir(&path).map_err(failed)?;
                write_tree(repository, id, target, &relative, links, symlinks)?;
            }
            EntryKind::Commit => {
                std::fs::create_dir(&path).map_err(failed)?;
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
                    symlinks.push((relative, link.to_owned()));
                    continue;
                }
                let mut file = std::fs::File::create_new(&path).map_err(failed)?;
                std::io::Write::write_all(&mut file, &blob.data).map_err(failed)?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::Permissions::from_mode(0o755);
                    file.set_permissions(mode).map_err(failed)?;
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

    #[test]
    fn answers_own_host() {
        let account = Some(("git".to_owned(), "token".to_owned()));
        let ask = |url: &str| {
            let context = gix::credentials::protocol::Context {
                url: Some(url.into()),
                ..Default::default()
            };
            let mut respond = answer("https://git.example.org/team/pipeline.git", account.clone());
            respond(gix::credentials::helper::Action::Get(context)).unwrap()
        };
        assert!(ask("https://git.example.org/team/tools.git").is_some());
        assert!(ask("https://other-host/x.git").is_none());
        assert!(ask("http://git.example.org/team/tools.git").is_none());
        assert!(ask("https://git.example.org:8443/team/tools.git").is_none());
    }

    #[test]
    fn submodules_ask_separately() {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let accounts = Accounts(Arc::new(move |url: &str| {
            seen.lock().unwrap().push(url.to_owned());
            Ok(url
                .starts_with("https://git.example.org/")
                .then(|| ("git".to_owned(), "token".to_owned())))
        }));
        let fetch = |url: &str| Fetch {
            url: url.into(),
            accounts: Some(accounts.clone()),
            ..Fetch::default()
        };
        assert!(
            account_of(&fetch("https://git.example.org/x.git"))
                .unwrap()
                .is_some()
        );
        assert!(
            account_of(&fetch("https://other-host/x.git"))
                .unwrap()
                .is_none()
        );
        assert!(
            account_of(&fetch("http://git.example.org/x.git"))
                .unwrap()
                .is_none()
        );
        let asked = asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            ["https://git.example.org/x.git", "https://other-host/x.git"]
        );
    }

    #[test]
    fn derives_codecommit_account() {
        let url = "https://git-codecommit.us-east-2.amazonaws.com/v1/repos/MyDemoRepo";
        assert_eq!(codecommit(url), Ok(true));
        assert_eq!(codecommit("https://git.example.org/x.git"), Ok(false));
        assert_eq!(
            codecommit("https://git-codecommit.a.b.amazonaws.com/v1/repos/x"),
            Err("Invalid AWS CodeCommit hostname: 'git-codecommit.a.b.amazonaws.com'".into())
        );
        let time: jiff::Timestamp = "2026-11-06T11:22:33Z".parse().unwrap();
        let expected =
            "20261106T112233Z058a08801f361d45784a3678fc492bcf211059e494a161875ad1077a4986fc10";
        let keys = (
            "AKIAIOSFODNN7EXAMPLE".to_owned(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
        );
        let none = |_: &str| None;
        let (user, password) = codecommit_account(url, Some(keys), &none, time).unwrap();
        assert_eq!(
            (user.as_str(), password.as_str()),
            ("AKIAIOSFODNN7EXAMPLE", expected)
        );
        // Without keys in the settings, the chain reads them from the environment.
        let environment = |name: &str| match name {
            "AWS_ACCESS_KEY_ID" => Some("AKIAIOSFODNN7EXAMPLE".to_owned()),
            "AWS_SECRET_ACCESS_KEY" => Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned()),
            "AWS_SESSION_TOKEN" => Some("session".to_owned()),
            "AWS_EC2_METADATA_DISABLED" => Some("true".to_owned()),
            _ => None,
        };
        let (user, password) = codecommit_account(url, None, &environment, time).unwrap();
        assert_eq!(user, "AKIAIOSFODNN7EXAMPLE%session");
        assert_eq!(password, expected);
    }

    #[test]
    fn never_writes_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let repository = bare(&root.path().join("repository")).unwrap();
        let entry = |kind: EntryKind, name: &str, oid| gix::objs::tree::Entry {
            mode: kind.into(),
            filename: name.into(),
            oid,
        };
        let file = repository.write_blob("evil").unwrap().detach();
        let inner = gix::objs::Tree {
            entries: vec![entry(EntryKind::Blob, ".bashrc", file)],
        };
        let inner = repository.write_object(&inner).unwrap().detach();
        let link = repository.write_blob(outside.to_str().unwrap()).unwrap();
        let write = |entries| {
            let tree = repository
                .write_object(&gix::objs::Tree { entries })
                .unwrap();
            let checkout = tempfile::tempdir_in(root.path()).unwrap();
            let mut symlinks = Vec::new();
            write_tree(
                &repository,
                tree.detach(),
                checkout.path(),
                Path::new(""),
                &mut Vec::new(),
                &mut symlinks,
            )
            .and_then(|()| write_links(checkout.path(), symlinks))
            .map(|()| checkout)
        };
        // A link `a` to a folder outside, then `a/.bashrc` through it.
        let twice = vec![
            entry(EntryKind::Link, "a", link.detach()),
            entry(EntryKind::Tree, "a", inner),
        ];
        assert!(write(twice).unwrap_err().contains("twice"));
        assert!(!outside.join(".bashrc").exists());
        let apart = vec![
            entry(EntryKind::Link, "a", link.detach()),
            entry(EntryKind::Tree, "b", inner),
        ];
        let checkout = write(apart).unwrap();
        assert!(checkout.path().join("a").is_symlink());
        assert_eq!(
            std::fs::read(checkout.path().join("b/.bashrc")).unwrap(),
            b"evil"
        );
        assert!(!outside.join(".bashrc").exists());
        let nested = vec![("a/z".into(), "y".to_owned())];
        assert!(write_links(checkout.path(), nested).is_err());
        assert!(!outside.join("z").exists());
    }

    #[test]
    fn contains_outward_links() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret");
        std::fs::write(&secret, "host").unwrap();
        let checkout = root.path().join("checkout");
        std::fs::create_dir_all(checkout.join("sub")).unwrap();
        std::fs::write(checkout.join("data"), "data").unwrap();
        std::os::unix::fs::symlink(&secret, checkout.join("leak")).unwrap();
        std::os::unix::fs::symlink(".", checkout.join("a")).unwrap();
        std::os::unix::fs::symlink("../secret", checkout.join("a/up")).unwrap();
        std::os::unix::fs::symlink("../data", checkout.join("sub/near")).unwrap();
        // `b` looks inside by name, but `c` leads to the top first.
        std::os::unix::fs::symlink("c/../secret", checkout.join("b")).unwrap();
        std::os::unix::fs::symlink("sub/..", checkout.join("c")).unwrap();
        contain_links(&checkout).unwrap();
        for (name, target) in [("leak", secret.to_str().unwrap()), ("up", "../secret")] {
            let place = checkout.join(name);
            assert!(!place.is_symlink(), "{name}");
            assert_eq!(std::fs::read_to_string(place).unwrap(), target);
        }
        assert!(checkout.join("sub/near").is_symlink());
        assert_eq!(
            std::fs::read_to_string(checkout.join("sub/near")).unwrap(),
            "data"
        );
        assert!(checkout.join("a").is_symlink());
        assert!(checkout.join("c").is_symlink());
        assert!(!checkout.join("b").is_symlink());
    }
}
