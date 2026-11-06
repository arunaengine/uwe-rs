//! Reads Quilt packages of `quilt+s3://` addresses through their manifests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::Problem;
use crate::objects::{Objects, Service, problem};
use crate::quilt_package::{self, Entry as Item};
use crate::{Downloaded, Entry, Metadata};

/// A Quilt+ address: a package in a bucket, maybe of one version, and a path inside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Address {
    pub bucket: String,
    /// `prefix/suffix`.
    pub package: String,
    /// The top hash after `@`.
    pub hash: Option<String>,
    /// The tag after `:`.
    pub tag: Option<String>,
    /// The logical path inside the package, `/`-separated; empty for the package itself.
    pub path: String,
    /// The pairs of the query, which become package metadata.
    pub meta: Vec<(String, String)>,
    pub workflow: Option<String>,
    pub catalog: Option<String>,
    pub force: Option<String>,
}

impl Address {
    /// Parses `quilt+s3://<bucket>[?<meta>]#package=<prefix>/<suffix>[@<hash>|:<tag>]&path=..`.
    pub fn parse(text: &str) -> Result<Address, String> {
        let rest = (text.strip_prefix("quilt+s3://")).ok_or_else(|| {
            format!("{text} is not a Quilt+ address, which starts with quilt+s3://")
        })?;
        let (before, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (bucket, query) = before.split_once('?').unwrap_or((before, ""));
        let bucket = bucket.trim_end_matches('/').to_lowercase();
        if bucket.is_empty() {
            return Err(format!("{text} names no bucket"));
        }
        let mut address = Address {
            bucket,
            meta: pairs(query),
            ..Address::default()
        };
        let mut path = String::new();
        for (key, value) in pairs(fragment) {
            match key.as_str() {
                "package" if value.contains('/') => address.package = value,
                "package" => tracing::error!("Invalid package[{value}]"),
                "path" => path = value,
                "workflow" => address.workflow = Some(value),
                "catalog" => address.catalog = Some(value),
                "force" => address.force = Some(value),
                _ => {}
            }
        }
        let package = std::mem::take(&mut address.package);
        let name = match package.find(['@', ':']) {
            Some(at) => {
                let version = package[at + 1..].to_owned();
                match package.as_bytes()[at] {
                    b'@' => address.hash = Some(version),
                    _ => address.tag = Some(version),
                }
                &package[..at]
            }
            None => package.as_str(),
        };
        let mut parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() < 2 {
            if parts.is_empty() {
                tracing::warn!("QuiltID.fetch: setting missing package to package/default");
                parts.push("package");
            }
            parts.push("default");
        }
        address.package = parts[..2].join("/");
        let mut logical: Vec<&str> = parts[2..].to_vec();
        logical.extend(path.split('/'));
        address.path = normalised(&logical);
        Ok(address)
    }

    /// The printed form, with `/` of the package and the path as `%2f`.
    pub fn text(&self) -> String {
        let mut text = format!("quilt+s3://{}", self.bucket);
        if !self.meta.is_empty() {
            let meta: Vec<String> = (self.meta.iter())
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            text = format!("{text}?{}", meta.join("&"));
        }
        text.push_str(&format!("#package={}", self.package.replace('/', "%2f")));
        match (&self.hash, &self.tag) {
            (Some(hash), _) => text.push_str(&format!("@{hash}")),
            (None, Some(tag)) => text.push_str(&format!(":{tag}")),
            (None, None) => {}
        }
        if !self.path.is_empty() {
            text.push_str(&format!("&path={}", self.path.replace('/', "%2f")));
        }
        for (key, value) in [("workflow", &self.workflow), ("catalog", &self.catalog)] {
            if let Some(value) = value {
                text.push_str(&format!("&{key}={value}"));
            }
        }
        text
    }
}

/// The entries of each manifest read so far, by bucket and top hash.
type Manifests = HashMap<(String, String), Arc<Vec<Item>>>;

pub(crate) struct Quilt {
    objects: Arc<Objects>,
    manifests: tokio::sync::Mutex<Manifests>,
}

impl Quilt {
    pub fn new(service: Service) -> Result<Quilt, Problem> {
        Ok(Quilt {
            objects: Arc::new(Objects::new(service)?),
            manifests: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// An entry of the package version, or a directory for the package and its folders.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let address = Address::parse(target).map_err(Problem::fixed)?;
        let Some(items) = self.items(&address).await? else {
            return Ok(None);
        };
        if let Some(item) = items.iter().find(|item| item.logical_key == address.path) {
            return Ok(Some(Metadata {
                directory: false,
                modified: None,
                size: item.size,
                version: Some(item.hash.clone()),
            }));
        }
        let folder = format!("{}/", address.path);
        let inside = address.path.is_empty()
            || items
                .iter()
                .any(|item| item.logical_key.starts_with(&folder));
        Ok(inside.then_some(Metadata {
            directory: true,
            modified: None,
            size: 0,
            version: None,
        }))
    }

    /// The entries and folders one level below a path of the package version.
    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let address = Address::parse(target).map_err(Problem::fixed)?;
        let items = self.items(&address).await?.ok_or_else(Problem::missing)?;
        let prefix = match address.path.as_str() {
            "" => String::new(),
            path => format!("{path}/"),
        };
        let mut entries: Vec<Entry> = Vec::new();
        for item in items.iter() {
            let Some(rest) = item.logical_key.strip_prefix(&prefix) else {
                continue;
            };
            let (name, directory) = match rest.split_once('/') {
                Some((folder, _)) => (folder, true),
                None => (rest, false),
            };
            if !entries.iter().any(|entry| entry.name == name) {
                entries.push(Entry {
                    name: name.to_owned(),
                    directory,
                });
            }
        }
        Ok(entries)
    }

    /// Downloads an entry from its physical key.
    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let address = Address::parse(target).map_err(Problem::fixed)?;
        let items = self.items(&address).await?.ok_or_else(Problem::missing)?;
        let item = (items.iter())
            .find(|item| item.logical_key == address.path)
            .ok_or_else(Problem::missing)?;
        self.objects.download(&item.physical_key, file, 0).await?;
        Ok(Downloaded {
            version: Some(item.hash.clone()),
        })
    }

    /// The entries of the package version of an address; `None` when it has no manifest.
    async fn items(&self, address: &Address) -> Result<Option<Arc<Vec<Item>>>, Problem> {
        let operator = self.objects.at(&format!("s3://{}/", address.bucket))?;
        let hash = match &address.hash {
            Some(hash) => hash.clone(),
            None => {
                let tag = address.tag.as_deref().unwrap_or("latest");
                let pointer = format!(".quilt/named_packages/{}/{tag}", address.package);
                match read(&operator, &pointer).await? {
                    Some(hash) => hash.trim().to_owned(),
                    None => return Ok(None),
                }
            }
        };
        let key = (address.bucket.clone(), hash.clone());
        let mut manifests = self.manifests.lock().await;
        if let Some(items) = manifests.get(&key) {
            return Ok(Some(items.clone()));
        }
        let Some(text) = read(&operator, &format!(".quilt/packages/{hash}")).await? else {
            return Ok(None);
        };
        let items = Arc::new(quilt_package::entries(&text).map_err(Problem::fixed)?);
        manifests.insert(key, items.clone());
        Ok(Some(items))
    }
}

/// The text of a small object; `None` when it is missing.
pub(crate) async fn read(
    operator: &opendal::Operator,
    key: &str,
) -> Result<Option<String>, Problem> {
    match operator.read(key).await {
        Ok(buffer) => Ok(Some(String::from_utf8_lossy(&buffer.to_vec()).into_owned())),
        Err(error) if error.kind() == opendal::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(problem(error)),
    }
}

/// The `key=value` pairs of a query or fragment, decoded; pairs without exactly one `=` are
/// dropped.
fn pairs(text: &str) -> Vec<(String, String)> {
    (text.split('&'))
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (!value.contains('='))
                .then(|| (crate::address::decode(key), crate::address::decode(value)))
        })
        .collect()
}

/// A path with `.` and empty parts dropped, and `..` dropped with the part before it.
fn normalised(parts: &[&str]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for part in parts {
        match *part {
            "" | "." => {}
            ".." => {
                kept.pop();
            }
            part => kept.push(part),
        }
    }
    kept.join("/")
}

/// The service of the built-in provider: S3 with the AWS settings of the environment.
pub(crate) fn environment() -> Service {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let endpoint = crate::aws::endpoint("S3", String::new(), variable);
    let endpoint = (!endpoint.is_empty()).then_some(endpoint);
    Service::S3 {
        region: (variable("AWS_REGION").or_else(|| variable("AWS_DEFAULT_REGION")))
            .unwrap_or_else(|| "us-east-1".to_owned()),
        virtual_host: endpoint.is_none(),
        endpoint,
        keys: None,
        anonymous: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quilt_addresses() {
        let text = "quilt+s3://NFQuilt-Test?team=a&run=1#package=test/probe&path=out/a.txt";
        let address = Address::parse(text).expect("an address");
        assert_eq!(
            (
                address.bucket.as_str(),
                address.package.as_str(),
                address.path.as_str()
            ),
            ("nfquilt-test", "test/probe", "out/a.txt")
        );
        assert_eq!(
            address.meta,
            [("team".into(), "a".into()), ("run".into(), "1".into())]
        );
        assert_eq!(
            address.text(),
            "quilt+s3://nfquilt-test?team=a&run=1#package=test%2fprobe&path=out%2fa.txt"
        );
        let pinned = Address::parse("quilt+s3://b#package=test/probe@f5f9&path=out%2Ftable.tsv");
        let pinned = pinned.expect("an address");
        assert_eq!(
            (pinned.hash.as_deref(), pinned.path.as_str()),
            (Some("f5f9"), "out/table.tsv")
        );
        let tagged = Address::parse("quilt+s3://b#package=a/b:v1").expect("an address");
        assert_eq!((tagged.tag.as_deref(), tagged.hash), (Some("v1"), None));
        let deep = Address::parse("quilt+s3://b#package=a/b/c/d&path=../e.txt").expect("parsed");
        assert_eq!(
            (deep.package.as_str(), deep.path.as_str()),
            ("a/b", "c/e.txt")
        );
        let lone = Address::parse("quilt+s3://b#package=solo").expect("parsed");
        assert_eq!(lone.package, "package/default");
        let short = Address::parse("quilt+s3://b#package=x/&path=a").expect("parsed");
        assert_eq!(short.package, "x/default");
        assert!(Address::parse("s3://b/a").is_err());
    }
}
