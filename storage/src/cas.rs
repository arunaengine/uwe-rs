//! Reads and writes content-addressed stores of `cas://` addresses: blocks named by their CIDs,
//! publish coordinates that name them, and the Store Log, in local folders or S3 prefixes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::cbor::{Cid, Node, RAW, decode};
use crate::error::Problem;
use crate::objects::{Objects, problem};
use crate::{Downloaded, Entry, Metadata, StorageError, Written};

/// The stores of a client: each member by its alias and location, the order reads look in, and
/// the member that writes go to.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub stores: Vec<(String, String)>,
    /// Aliases in the order reads look in; all, writable first, without one.
    pub resolve: Vec<String>,
    pub writable: Option<String>,
}

/// The options of the `cas` scope of a Nextflow config, for `cas://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `cas://` addresses that no provider of the config file serves, and
/// for the functions of this module, from the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

/// Writes a DAG-CBOR block into the writable store of [`use_scope`] and returns its CID.
pub fn put_block(node: &Node) -> Result<Cid, StorageError> {
    let node = node.clone();
    run("write a block to", move |cas| async move {
        let (bytes, cid) = node.block().map_err(Problem::fixed)?;
        cas.writable()?.absent(&block_key(&cid), &bytes).await?;
        Ok(cid)
    })
}

/// Writes the Store Log entry `log/<rts>-<kind>-<cid>`, newest first by name.
pub fn put_log(kind: &str, cid: &Cid) -> Result<(), StorageError> {
    let key = format!(
        "log/{:013}-{kind}-{cid}",
        9_999_999_999_999 - jiff::Timestamp::now().as_millisecond()
    );
    run("write the Store Log of", move |cas| async move {
        cas.writable()?.absent(&key, b"").await
    })
}

/// A block of the stores of [`use_scope`], decoded; raw blocks are refused.
pub fn block(cid: &Cid) -> Result<Option<Node>, StorageError> {
    let cid = *cid;
    run("read a block of", move |cas| async move {
        match cas.find(&cid).await? {
            Some(bytes) => decode(&bytes).map(Some).map_err(Problem::fixed),
            None => Ok(None),
        }
    })
}

/// The CIDs that the `run` entries of the Store Logs name, newest first, across the stores.
pub fn runs() -> Result<Vec<Cid>, StorageError> {
    run("read the Store Log of", |cas| async move {
        let mut entries = Vec::new();
        for (_, member) in &cas.members {
            entries.extend(
                (member.entries("log").await?)
                    .into_iter()
                    .map(|entry| entry.name),
            );
        }
        entries.sort();
        entries.dedup();
        Ok((entries.iter())
            .filter_map(|name| Cid::parse(name.split_once("-run-")?.1))
            .collect())
    })
}

/// The content and size that a publish coordinate `cas://<alias>/<path>` names.
pub fn coordinate(target: &str) -> Result<Option<(Cid, u64)>, StorageError> {
    let target = target.to_owned();
    run("read the coordinate", move |cas| async move {
        let Some(content) = cas.coordinate(&target).await? else {
            return Ok(None);
        };
        let size = cas.size(&content).await?.unwrap_or(0);
        Ok(Some((content, size)))
    })
}

/// Runs a request against the stores of [`use_scope`].
fn run<T: Send + 'static, F>(
    action: &'static str,
    request: impl FnOnce(Cas) -> F + Send + 'static,
) -> Result<T, StorageError>
where
    F: Future<Output = Result<T, Problem>> + Send,
{
    crate::blocking::run(async move {
        let failed = |problem: Problem| StorageError::Failed {
            action,
            address: "cas://".to_owned(),
            provider: "the stores of the cas scope".to_owned(),
            message: problem.message,
            retryable: problem.retryable,
        };
        let cas = Cas::new(scope()).map_err(failed)?;
        request(cas).await.map_err(failed)
    })
}

pub(crate) struct Cas {
    /// The members in the order reads look in.
    members: Vec<(String, Member)>,
    writable: Option<String>,
}

/// A member store: a folder on this machine or a place in S3.
enum Member {
    Folder(PathBuf),
    Objects(opendal::Operator),
}

/// What a Store URI names.
enum Named {
    /// A file: its content block.
    Content(Cid),
    /// A block itself, read as a file named by its CID.
    Block(Cid),
    /// A folder of publish coordinates.
    Folder(Vec<Entry>),
}

impl Cas {
    pub fn new(options: Options) -> Result<Cas, Problem> {
        let mut order: Vec<String> = options.resolve.clone();
        if order.is_empty() {
            order.extend(options.writable.clone());
            for (alias, _) in &options.stores {
                if !order.contains(alias) {
                    order.push(alias.clone());
                }
            }
        }
        let mut members = Vec::new();
        for alias in order {
            let location = (options.stores.iter())
                .find(|(name, _)| *name == alias)
                .map(|(_, location)| location.clone())
                .ok_or_else(|| {
                    Problem::fixed(format!("cas.resolve names the unknown store {alias}"))
                })?;
            let member = match crate::address::scheme(&location) {
                None => Member::Folder(PathBuf::from(location)),
                Some("s3") => {
                    let objects = Objects::new(crate::quilt::environment())?;
                    Member::Objects(objects.at(&location)?)
                }
                Some(_) => {
                    return Err(Problem::fixed(format!(
                        "the store {alias} must be a local folder or an s3:// prefix, not {location}"
                    )));
                }
            };
            members.push((alias, member));
        }
        Ok(Cas {
            members,
            writable: options.writable,
        })
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        match self.named(target).await? {
            Some(Named::Content(cid) | Named::Block(cid)) => {
                Ok(self.size(&cid).await?.map(|size| Metadata {
                    directory: false,
                    modified: None,
                    size,
                    version: Some(cid.to_string()),
                }))
            }
            Some(Named::Folder(_)) => Ok(Some(Metadata {
                directory: true,
                modified: None,
                size: 0,
                version: None,
            })),
            None => Ok(None),
        }
    }

    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        match self.named(target).await?.ok_or_else(Problem::missing)? {
            Named::Folder(entries) => Ok(entries),
            _ => Err(Problem::fixed("a file is not a folder")),
        }
    }

    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let (Named::Content(cid) | Named::Block(cid)) =
            self.named(target).await?.ok_or_else(Problem::missing)?
        else {
            return Err(Problem::fixed("a folder of the store is not a file"));
        };
        let bytes = self.find(&cid).await?.ok_or_else(Problem::missing)?;
        std::fs::write(file, bytes).map_err(|error| Problem::local(file, error))?;
        Ok(Downloaded {
            version: Some(cid.to_string()),
        })
    }

    /// Publishes a file at a coordinate of the writable store: its content block, then the
    /// coordinate, which names the content.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<Written, Problem> {
        let (alias, path) = coordinate_parts(target)?;
        if self.writable.as_deref() != Some(alias) {
            return Err(Problem::fixed(format!(
                "cas://{alias} is not the writable store; publish to the store of \
                 lineage.store.location"
            )));
        }
        let path = normalised(path);
        let name = path.rsplit('/').next().unwrap_or_default().to_owned();
        if name.is_empty() {
            return Err(Problem::fixed(format!("{target} names no file")));
        }
        let bytes = std::fs::read(file).map_err(|error| Problem::local(file, error))?;
        let cid = Cid::of(RAW, &bytes);
        let member = self.writable()?;
        member.absent(&block_key(&cid), &bytes).await?;
        let line = format!("cas://{cid}/{name}\n");
        member
            .replace(&format!("coords/{path}"), line.as_bytes())
            .await?;
        Ok(Written {
            size: bytes.len() as u64,
            version: Some(cid.to_string()),
        })
    }

    fn writable(&self) -> Result<&Member, Problem> {
        let alias = (self.writable.as_deref())
            .ok_or_else(|| Problem::fixed("no store is writable; set lineage.store.location"))?;
        (self.members.iter())
            .find(|(name, _)| name == alias)
            .map(|(_, member)| member)
            .ok_or_else(|| Problem::fixed(format!("the writable store {alias} is not configured")))
    }

    /// The bytes of a block, from the first member that has it.
    async fn find(&self, cid: &Cid) -> Result<Option<Vec<u8>>, Problem> {
        for (_, member) in &self.members {
            if let Some(bytes) = member.read(&block_key(cid)).await? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    async fn size(&self, cid: &Cid) -> Result<Option<u64>, Problem> {
        Ok(self.find(cid).await?.map(|bytes| bytes.len() as u64))
    }

    /// The content that a coordinate names, from the member of its alias.
    async fn coordinate(&self, target: &str) -> Result<Option<Cid>, Problem> {
        let (alias, path) = coordinate_parts(target)?;
        let member = (self.members.iter())
            .find(|(name, _)| name == alias)
            .map(|(_, member)| member)
            .ok_or_else(|| Problem::fixed(format!("cas://{alias} is not a configured store")))?;
        let Some(line) = member.read(&format!("coords/{}", normalised(path))).await? else {
            return Ok(None);
        };
        let line = String::from_utf8_lossy(&line);
        let cid = (line.trim().strip_prefix("cas://"))
            .and_then(|rest| Cid::parse(rest.split('/').next()?))
            .ok_or_else(|| Problem::fixed(format!("the coordinate {target} names no CID")))?;
        Ok(Some(cid))
    }

    /// What an address names: a coordinate, or a Store URI traversed by its segments.
    async fn named(&self, target: &str) -> Result<Option<Named>, Problem> {
        let rest = target
            .strip_prefix("cas://")
            .ok_or_else(|| Problem::fixed("not a cas:// address"))?;
        let mut segments = rest.split('/').filter(|segment| !segment.is_empty());
        let first = segments.next().unwrap_or_default();
        let segments: Vec<&str> = segments.collect();
        let Some(cid) = Cid::parse(first) else {
            if let Some(cid) = self.coordinate(target).await? {
                return Ok(Some(Named::Content(cid)));
            }
            let (_, path) = coordinate_parts(target)?;
            let folder = format!("coords/{}", normalised(path));
            let Some((_, member)) = self.members.iter().find(|(alias, _)| alias == first) else {
                return Ok(None);
            };
            let entries = member.entries(&folder).await?;
            return Ok((!entries.is_empty()).then_some(Named::Folder(entries)));
        };
        self.traverse(cid, &segments).await
    }

    /// Follows the segments of a Store URI from a block.
    async fn traverse(&self, cid: Cid, segments: &[&str]) -> Result<Option<Named>, Problem> {
        if cid.codec == RAW || segments.is_empty() {
            return Ok(match segments {
                [] => Some(Named::Block(cid)),
                [_] => Some(Named::Content(cid)),
                _ => None,
            });
        }
        let Some(bytes) = self.find(&cid).await? else {
            return Ok(None);
        };
        let node = decode(&bytes).map_err(Problem::fixed)?;
        match node.get("kind").and_then(Node::text) {
            Some("OutputItem") => {
                let leaves = leaves(&node);
                let found = leaves.iter().find(|(name, _)| name == segments[0]);
                Ok(match (found, &segments[1..]) {
                    (Some((_, Some(content))), []) => Some(Named::Content(*content)),
                    _ => None,
                })
            }
            Some("OutputCollection") => {
                let items = links(node.get("items"));
                if let Some(item) = Cid::parse(segments[0]).filter(|item| items.contains(item)) {
                    return Box::pin(self.traverse(item, &segments[1..])).await;
                }
                let wanted = segments.join("/");
                let paths = match node.get("paths") {
                    Some(Node::List(paths)) => paths.clone(),
                    _ => Vec::new(),
                };
                for (item, paths) in items.iter().zip(paths) {
                    let Node::List(paths) = paths else { continue };
                    if paths
                        .iter()
                        .any(|path| path.text() == Some(wanted.as_str()))
                    {
                        let name = segments[segments.len() - 1];
                        return Box::pin(self.traverse(*item, &[name])).await;
                    }
                }
                Ok(None)
            }
            Some("RunCompletion") => {
                for collection in links(node.get("collections")) {
                    if let Some(found) = Box::pin(self.traverse(collection, segments)).await? {
                        return Ok(Some(found));
                    }
                }
                Ok(None)
            }
            _ => Err(Problem::fixed(format!(
                "uwe cannot follow a path inside the block {cid}"
            ))),
        }
    }
}

impl Member {
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, Problem> {
        match self {
            Member::Folder(root) => match std::fs::read(root.join(key)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::IsADirectory
                            | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(Problem::local(&root.join(key), error)),
            },
            Member::Objects(operator) => match operator.read(key).await {
                Ok(buffer) => Ok(Some(buffer.to_vec())),
                Err(error) if error.kind() == opendal::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(problem(error)),
            },
        }
    }

    /// Writes a block that never changes: an existing one stays, and a local one is made
    /// read-only and linked into place whole.
    async fn absent(&self, key: &str, bytes: &[u8]) -> Result<(), Problem> {
        match self {
            Member::Folder(root) => {
                let target = root.join(key);
                if target.exists() {
                    return Ok(());
                }
                let parent = target.parent().unwrap_or(root);
                let local = |error| Problem::local(&target, error);
                std::fs::create_dir_all(parent).map_err(local)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(local)?;
                temporary.write_all(bytes).map_err(local)?;
                temporary.as_file().sync_all().map_err(local)?;
                read_only(temporary.path()).map_err(local)?;
                match std::fs::hard_link(temporary.path(), &target) {
                    Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                        Err(local(error))
                    }
                    _ => Ok(()),
                }
            }
            Member::Objects(operator) => {
                let written = operator
                    .write_with(key, bytes.to_vec())
                    .if_not_exists(true)
                    .cache_control("public, max-age=31536000, immutable")
                    .await;
                match written {
                    Err(error)
                        if matches!(
                            error.kind(),
                            opendal::ErrorKind::ConditionNotMatch
                                | opendal::ErrorKind::AlreadyExists
                        ) =>
                    {
                        Ok(())
                    }
                    other => other.map(|_| ()).map_err(problem),
                }
            }
        }
    }

    async fn replace(&self, key: &str, bytes: &[u8]) -> Result<(), Problem> {
        match self {
            Member::Folder(root) => {
                let target = root.join(key);
                let local = |error| Problem::local(&target, error);
                std::fs::create_dir_all(target.parent().unwrap_or(root)).map_err(local)?;
                std::fs::write(&target, bytes).map_err(local)
            }
            Member::Objects(operator) => operator
                .write(key, bytes.to_vec())
                .await
                .map(|_| ())
                .map_err(problem),
        }
    }

    /// The entries of a folder of the store.
    async fn entries(&self, folder: &str) -> Result<Vec<Entry>, Problem> {
        match self {
            Member::Folder(root) => match std::fs::read_dir(root.join(folder)) {
                Ok(entries) => Ok(entries
                    .flatten()
                    .map(|entry| Entry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        directory: entry.file_type().is_ok_and(|kind| kind.is_dir()),
                    })
                    .collect()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(error) => Err(Problem::local(&root.join(folder), error)),
            },
            Member::Objects(operator) => {
                let prefix = format!("{}/", folder.trim_end_matches('/'));
                match operator.list(&prefix).await {
                    Ok(entries) => Ok(entries
                        .into_iter()
                        .filter(|entry| entry.path() != prefix)
                        .map(|entry| Entry {
                            name: entry.name().trim_end_matches('/').to_owned(),
                            directory: entry.metadata().is_dir(),
                        })
                        .collect()),
                    Err(error) if error.kind() == opendal::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(error) => Err(problem(error)),
                }
            }
        }
    }
}

/// The leaves of an item value: their names and the content they name, `None` for a leaf
/// that names none.
pub fn leaves(node: &Node) -> Vec<(String, Option<Cid>)> {
    let mut found = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            Node::Map(_) if node.get("kind").and_then(Node::text) == Some("Leaf") => {
                let name = node.get("name").and_then(Node::text).unwrap_or_default();
                let content = match node.get("address") {
                    Some(Node::Link(cid)) => Some(*cid),
                    _ => None,
                };
                found.push((name.to_owned(), content));
            }
            Node::Map(entries) => stack.extend(entries.iter().rev().map(|(_, value)| value)),
            Node::List(items) => stack.extend(items.iter().rev()),
            _ => {}
        }
    }
    found
}

fn links(node: Option<&Node>) -> Vec<Cid> {
    match node {
        Some(Node::List(items)) => (items.iter())
            .filter_map(|item| match item {
                Node::Link(cid) => Some(*cid),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `blocks/<last two characters>/<cid>`.
fn block_key(cid: &Cid) -> String {
    let text = cid.to_string();
    format!("blocks/{}/{text}", &text[text.len() - 2..])
}

/// The alias and the path of `cas://<alias>/<path>`.
fn coordinate_parts(target: &str) -> Result<(&str, &str), Problem> {
    let rest = target
        .strip_prefix("cas://")
        .ok_or_else(|| Problem::fixed("not a cas:// address"))?;
    Ok(rest.split_once('/').unwrap_or((rest, "")))
}

/// A path with `.`, empty parts and `..` resolved.
fn normalised(path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                kept.pop();
            }
            part => kept.push(part),
        }
    }
    kept.join("/")
}

#[cfg(unix)]
fn read_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
}
