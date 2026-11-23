//! Checks a host provider of `aruna://` addresses: routing, digests, trees and transfers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use sha2::{Digest as _, Sha256};
use storage::{
    Details, DirectRef, Entry, Metadata, Storage, StorageError, StorageProvider, UploadReceipt,
};

/// Objects in memory by their address, with a count of downloads.
#[derive(Default)]
struct Aruna {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    downloads: Mutex<usize>,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Aruna {
    fn with(objects: &[(&str, &str)]) -> Arc<Aruna> {
        let aruna = Aruna::default();
        let mut stored = aruna.objects.lock().unwrap();
        for (address, text) in objects {
            stored.insert((*address).to_owned(), text.as_bytes().to_vec());
        }
        drop(stored);
        Arc::new(aruna)
    }

    fn direct(address: &str) -> DirectRef {
        DirectRef {
            url: format!(
                "https://proxy.example.org/{}?signature=secret",
                &address[8..]
            ),
            headers: vec![("authorization".into(), "Bearer secret".into())],
            aruna_id: Some(format!("01J{}", address.len())),
            endpoint: Some("proxy.example.org".into()),
            encryption: None,
        }
    }
}

impl StorageProvider for Aruna {
    fn name(&self) -> &str {
        "aruna"
    }

    fn matches(&self, address: &str) -> bool {
        address.starts_with("aruna://")
    }

    fn stat<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Option<Details>, StorageError>> {
        Box::pin(async move {
            let objects = self.objects.lock().unwrap();
            if let Some(bytes) = objects.get(address) {
                return Ok(Some(Details {
                    metadata: Metadata {
                        directory: false,
                        modified: None,
                        size: bytes.len() as u64,
                        version: Some(hex(bytes)),
                    },
                    digest: Some(hex(bytes)),
                    endpoints: vec!["proxy.example.org".into()],
                    direct: Some(Aruna::direct(address)),
                }));
            }
            let folder = format!("{}/", address.trim_end_matches('/'));
            Ok(objects.keys().any(|key| key.starts_with(&folder)).then(|| {
                Details::of(Metadata {
                    directory: true,
                    modified: None,
                    size: 0,
                    version: None,
                })
            }))
        })
    }

    fn list<'a>(&'a self, address: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, StorageError>> {
        Box::pin(async move {
            let folder = format!("{}/", address.trim_end_matches('/'));
            let mut entries: Vec<Entry> = Vec::new();
            for key in self.objects.lock().unwrap().keys() {
                let Some(rest) = key.strip_prefix(&folder) else {
                    continue;
                };
                let (name, directory) = match rest.split_once('/') {
                    Some((name, _)) => (name, true),
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
        })
    }

    fn download<'a>(
        &'a self,
        address: &'a str,
        target: &'a Path,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
        Box::pin(async move {
            *self.downloads.lock().unwrap() += 1;
            let bytes = (self.objects.lock().unwrap().get(address).cloned()).ok_or_else(|| {
                StorageError::Missing {
                    address: address.to_owned(),
                    provider: "aruna".into(),
                }
            })?;
            std::fs::write(target, &bytes).unwrap();
            Ok(Some(hex(&bytes)))
        })
    }

    fn upload<'a>(
        &'a self,
        source: &'a Path,
        address: &'a str,
    ) -> BoxFuture<'a, Result<UploadReceipt, StorageError>> {
        Box::pin(async move {
            let bytes = std::fs::read(source).unwrap();
            let receipt = UploadReceipt {
                address: address.to_owned(),
                size: bytes.len() as u64,
                version: None,
                digest: Some(hex(&bytes)),
            };
            self.objects
                .lock()
                .unwrap()
                .insert(address.to_owned(), bytes);
            Ok(receipt)
        })
    }

    fn upload_absent<'a>(
        &'a self,
        source: &'a Path,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Option<UploadReceipt>, StorageError>> {
        Box::pin(async move {
            if self.objects.lock().unwrap().contains_key(address) {
                return Ok(None);
            }
            self.upload(source, address).await.map(Some)
        })
    }
}

#[tokio::test]
async fn resolves_host_addresses() {
    let aruna = Aruna::with(&[("aruna://project/reads.fq", "ACGT\n")]);
    let plain = Storage::default();
    assert!(matches!(
        plain.stat("aruna://project/reads.fq").await,
        Err(StorageError::NoProvider { .. })
    ));
    let storage = plain.with_provider(aruna.clone());
    let details = storage.describe("aruna://project/reads.fq").await.unwrap();
    let details = details.expect("the object");
    assert_eq!(details.verified(), Some(hex(b"ACGT\n").as_str()));
    assert_eq!(details.metadata.size, 5);
    assert_eq!(details.endpoints, ["proxy.example.org"]);
    let direct = storage
        .direct_ref("aruna://project/reads.fq")
        .await
        .unwrap();
    let shown = format!("{:?}", direct.expect("a direct reference"));
    assert!(!shown.contains("secret"), "{shown}");
    let found = storage.stat("aruna://project/reads.fq").await.unwrap();
    assert_eq!(found.map(|found| found.size), Some(5));
    assert_eq!(storage.stat("aruna://project/none").await.unwrap(), None);
    // The host provider stays in sessions, and its addresses are no store.
    assert!(
        storage
            .session()
            .stat("aruna://project/reads.fq")
            .await
            .is_ok()
    );
    assert!(storage.store("aruna://project/store").is_err());
    // Other addresses keep their own providers.
    let folder = tempfile::tempdir().unwrap();
    let local = format!("file://{}", folder.path().display());
    let described = storage.describe(&local).await.unwrap().expect("the folder");
    assert!(described.metadata.directory && described.digest.is_none());
}

#[tokio::test]
async fn walks_host_trees() {
    let aruna = Aruna::with(&[
        ("aruna://project/index/genome.fa", ">chr1\nACGT\n"),
        ("aruna://project/index/parts/a.bin", "a"),
        ("aruna://project/index/parts/b.bin", "bb"),
        ("aruna://project/other.txt", "other"),
    ]);
    let storage = Storage::default().with_provider(aruna);
    let manifest = storage
        .tree_manifest("aruna://project/index")
        .await
        .unwrap();
    let paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["genome.fa", "parts/a.bin", "parts/b.bin"]);
    assert_eq!(manifest.files[2].size, 2);
    assert_eq!(
        manifest.files[1].digest.as_deref(),
        Some(hex(b"a").as_str())
    );
    assert!(manifest.direct());
    assert!(manifest.empty.is_empty());
    // A built-in provider walks its folders too, without digests.
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(folder.path().join("tree/empty")).unwrap();
    std::fs::write(folder.path().join("tree/file.txt"), "file").unwrap();
    let address = format!("file://{}/tree", folder.path().display());
    let walked = storage.tree_manifest(&address).await.unwrap();
    assert_eq!(walked.files.len(), 1);
    assert_eq!(
        (walked.files[0].path.as_str(), walked.files[0].size),
        ("file.txt", 4)
    );
    assert_eq!(walked.empty, ["empty"]);
    assert!(!walked.direct());
}

#[tokio::test]
async fn transfers_host_files() {
    let aruna = Aruna::with(&[("aruna://project/reads.fq", "ACGT\n")]);
    let storage = Storage::default().with_provider(aruna.clone());
    let downloads = tempfile::tempdir().unwrap();
    let copy = storage
        .fetch("aruna://project/reads.fq", downloads.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"ACGT\n");
    // The same version is read from the copy of the downloads.
    let again = storage.session();
    let second = again
        .fetch("aruna://project/reads.fq", downloads.path())
        .await
        .unwrap();
    assert_eq!(second, copy);
    assert_eq!(*aruna.downloads.lock().unwrap(), 1);
    let source = downloads.path().join("result.txt");
    std::fs::write(&source, "result").unwrap();
    let receipt = storage
        .upload_with_receipt(&source, "aruna://project/result.txt")
        .await
        .unwrap();
    assert_eq!(receipt.size, 6);
    assert_eq!(receipt.digest.as_deref(), Some(hex(b"result").as_str()));
    let absent = storage
        .upload_absent(&source, "aruna://project/result.txt")
        .await
        .unwrap();
    assert!(!absent, "existing content stays");
    let fresh = storage
        .upload_absent(&source, "aruna://project/new.txt")
        .await
        .unwrap();
    assert!(fresh);
}

/// One file of a tenant at a fixed address and version, which the provider does not hash.
struct Tenant {
    text: &'static str,
    scope: Option<&'static str>,
    digest: Option<String>,
}

impl StorageProvider for Tenant {
    fn name(&self) -> &str {
        "aruna"
    }

    fn matches(&self, address: &str) -> bool {
        address.starts_with("aruna://")
    }

    fn cache_scope(&self) -> Option<String> {
        self.scope.map(str::to_owned)
    }

    fn stat<'a>(
        &'a self,
        _address: &'a str,
    ) -> BoxFuture<'a, Result<Option<Details>, StorageError>> {
        Box::pin(async move {
            Ok(Some(Details {
                digest: self.digest.clone(),
                ..Details::of(Metadata {
                    directory: false,
                    modified: None,
                    size: self.text.len() as u64,
                    version: Some("1".into()),
                })
            }))
        })
    }

    fn download<'a>(
        &'a self,
        _address: &'a str,
        target: &'a Path,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
        Box::pin(async move {
            std::fs::write(target, self.text).map_err(|error| StorageError::Local {
                path: target.to_owned(),
                source: error,
            })?;
            Ok(Some("1".into()))
        })
    }
}

/// Host providers of one name that serve other files at one address and version never share
/// a download in one folder, unless they name one scope and the same content.
#[tokio::test]
async fn separates_host_downloads() {
    let folder = tempfile::tempdir().unwrap();
    let address = "aruna://project/data.txt";
    let read = |tenant: Tenant| {
        let storage = Storage::default().with_provider(Arc::new(tenant));
        let downloads = folder.path().to_owned();
        async move {
            let copy = storage.fetch(address, &downloads).await.unwrap();
            std::fs::read_to_string(copy).unwrap()
        }
    };
    let tenant = |text, scope, digest: Option<&str>| Tenant {
        text,
        scope,
        digest: digest.map(str::to_owned),
    };
    assert_eq!(read(tenant("first\n", None, None)).await, "first\n");
    assert_eq!(read(tenant("second\n", None, None)).await, "second\n");
    // A scope keeps tenants apart; within one, a verified digest tells content apart.
    let (one, two) = (Some(hex(b"one\n")), Some(hex(b"two\n")));
    let scoped = |text, scope, digest: &Option<String>| tenant(text, scope, digest.as_deref());
    assert_eq!(read(scoped("one\n", Some("a"), &one)).await, "one\n");
    assert_eq!(read(scoped("two\n", Some("b"), &one)).await, "two\n");
    assert_eq!(read(scoped("two\n", Some("a"), &two)).await, "two\n");
    // The same scope and content share the download of the first.
    assert_eq!(read(scoped("ONE\n", Some("a"), &one)).await, "one\n");
}
