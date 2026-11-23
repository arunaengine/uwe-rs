//! The state of one run that its work shares, and the storage providers and run store of a host.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::sync::Arc;

use config_file::SecretValues;

use crate::hosted::Isolation;

/// What one run uses besides its arguments: its folders, storage, registry settings, isolation
/// and secrets. Work that the run starts on other threads and tasks carries it, so that runs of
/// one process with other settings stay apart.
#[derive(Clone)]
pub struct RunContext {
    /// The folder of downloads, scratch folders and input digests of the run.
    pub cache_folder: PathBuf,
    /// The folder of the journals of the containers that the run starts on this machine.
    pub journal_folder: PathBuf,
    pub storage: storage::Storage,
    pub registry: executor::credentials::Registry,
    pub isolation: Option<Isolation>,
    /// The values of the run for the variables that its jobs and secrets name.
    pub secrets: SecretValues,
    /// Where remote executors note the jobs of running attempts, so that a later process finds
    /// and stops what a run left: the host's store, else files in the scratch folder.
    pub journal_store: Arc<dyn executor::JournalStore>,
}

impl RunContext {
    /// The folder of the copies of the remote files that the run reads.
    pub fn downloads(&self) -> PathBuf {
        self.cache_folder.join("downloads")
    }

    /// The folder of the scratch folders of the tasks and drivers of the run.
    pub fn scratch(&self) -> PathBuf {
        self.cache_folder.join("scratch")
    }
}

/// The run store of the embedding host, which keeps the runs instead of the run database file,
/// as `Settings.run_store` gives it.
#[derive(Clone)]
pub struct HostRuns(pub(crate) Arc<dyn run_database::RunStore>);

impl std::fmt::Debug for HostRuns {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostRuns")
    }
}

/// Two run stores are equal when they are the same store.
impl PartialEq for HostRuns {
    fn eq(&self, other: &HostRuns) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A storage provider of the embedding host, like one of `aruna://` addresses. It serves the
/// addresses that it matches for the run, before the providers of the config file.
#[derive(Clone)]
pub struct HostStorage(pub(crate) Arc<dyn storage::StorageProvider>);

impl HostStorage {
    /// Wraps a provider, such as `Arc::new(provider)` of any [`storage::StorageProvider`].
    pub fn new(provider: Arc<dyn storage::StorageProvider>) -> HostStorage {
        HostStorage(provider)
    }

    /// The storage of a run with the providers of its host, the first given serving first.
    pub(crate) fn added(hosts: &[HostStorage], storage: &storage::Storage) -> storage::Storage {
        (hosts.iter().rev()).fold(storage.clone(), |storage, host| {
            storage.with_provider(host.0.clone())
        })
    }
}

impl std::fmt::Debug for HostStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "HostStorage({})", self.0.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use storage::{Details, Metadata, StorageError, StorageProvider};

    /// A provider that says its name as the version of every `aruna://` file.
    struct Named(&'static str);

    impl StorageProvider for Named {
        fn name(&self) -> &str {
            self.0
        }

        fn matches(&self, address: &str) -> bool {
            address.starts_with("aruna://")
        }

        fn stat<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<Details>, StorageError>> {
            Box::pin(async move {
                Ok(Some(Details::of(Metadata {
                    directory: false,
                    modified: None,
                    size: 1,
                    version: Some(self.0.to_owned()),
                })))
            })
        }

        fn download<'a>(
            &'a self,
            _: &'a str,
            _: &'a std::path::Path,
        ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
            Box::pin(async move { Ok(None) })
        }
    }

    #[tokio::test]
    async fn serves_first_host() {
        let hosts = [
            HostStorage::new(Arc::new(Named("first"))),
            HostStorage::new(Arc::new(Named("second"))),
        ];
        assert_eq!(format!("{:?}", hosts[0]), "HostStorage(first)");
        let storage = HostStorage::added(&hosts, &storage::Storage::default());
        let found = storage.stat("aruna://project/file").await.unwrap();
        assert_eq!(
            found.and_then(|found| found.version).as_deref(),
            Some("first")
        );
        let plain = HostStorage::added(&[], &storage::Storage::default());
        assert!(plain.stat("aruna://project/file").await.is_err());
    }
}
