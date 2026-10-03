//! Streams task objects into verified local files and exposes remote content addresses.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use sha2::{Digest as _, Sha256};
use task_model::Digest;
use tokio::io::AsyncWriteExt;

use crate::store::PART_SIZE;
use crate::{Store, StoreError, layout};

impl Store {
    /// The S3 address of existing content, including the shared layer of a local store.
    pub async fn content_address(&self, digest: &Digest) -> Result<Option<String>, StoreError> {
        let location = layout::content(digest);
        let mut layer = Some(self);
        while let Some(store) = layer {
            if store
                .base
                .as_deref()
                .is_some_and(|base| base.starts_with("s3://"))
                && store.operator.exists(&location).await?
            {
                return Ok(Some(store.url(&location)));
            }
            layer = store.shared.as_deref();
        }
        Ok(None)
    }

    /// Reads a task output atomically, checking its reported size and computing its digest.
    pub async fn read_object(
        &self,
        location: &str,
        version: Option<&str>,
        target: &Path,
        expected_size: Option<u64>,
    ) -> Result<(Digest, u64), StoreError> {
        let mut stat = self.operator.stat_with(location);
        let mut read = self.operator.reader_with(location);
        if let Some(version) = version {
            stat = stat.version(version);
            read = read.version(version);
        }
        let size = stat.await?.content_length();
        if expected_size.is_some_and(|expected| size != expected) {
            return Err(StoreError::Location(
                "TES output size differs from its log".into(),
            ));
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        tokio::fs::create_dir_all(parent).await?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        let mut file = tokio::fs::File::from_std(temporary.reopen()?);
        let reader = read.await?;
        let mut hash = Sha256::new();
        let mut offset = 0;
        while offset < size {
            let end = (offset + PART_SIZE as u64).min(size);
            let bytes = reader.read(offset..end).await?.to_bytes();
            if bytes.len() as u64 != end - offset {
                return Err(StoreError::Location(
                    "TES output changed size while being read".into(),
                ));
            }
            hash.update(&bytes);
            file.write_all(&bytes).await?;
            offset = end;
        }
        file.flush().await?;
        drop(file);
        temporary.persist(target).map_err(|error| error.error)?;
        Ok((Digest::from_sha256(&hash.finalize()), size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifies_object_size() {
        let store = Store::memory().unwrap();
        let digest = store.put_content(b"output".to_vec().into()).await.unwrap();
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("output");
        let location = layout::content(&digest);
        assert!(
            store
                .read_object(&location, None, &target, Some(7))
                .await
                .is_err()
        );
        assert!(!target.exists());
        assert_eq!(
            store
                .read_object(&location, None, &target, Some(6))
                .await
                .unwrap(),
            (digest, 6)
        );
        assert_eq!(std::fs::read(target).unwrap(), b"output");
    }
}
