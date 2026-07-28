//! The remote files that a run read, as the storage providers noted them: task inputs at
//! addresses, and files that workflow code read, like a sample sheet on the web. The run
//! database records them by digest, and a real run keeps their contents in its store, so that a
//! repeat of the run reads the same bytes, also when the file at the address has changed.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::UNIX_EPOCH;

use content_store::Store;
use run_database::RemoteFile;
use storage::{Fetched, Storage};

/// The remote files that the storage read, without the outputs of tasks that a store at an
/// address holds, which are no inputs of the run.
pub fn read(storage: &Storage, store_location: &str) -> Vec<Fetched> {
    let inside_store = |address: &str| {
        storage::address::scheme(store_location).is_some()
            && address.starts_with(&format!("{}/", store_location.trim_end_matches('/')))
    };
    (storage.fetched().into_iter())
        .filter(|fetched| !inside_store(&fetched.address))
        .collect()
}

/// The rows of the run database for the remote files that a run read.
pub fn rows(fetched: &[Fetched]) -> Vec<RemoteFile> {
    (fetched.iter())
        .map(|fetched| RemoteFile {
            address: fetched.address.clone(),
            digest: fetched.digest.clone(),
            size: fetched.size,
            fetched: (fetched.time.duration_since(UNIX_EPOCH))
                .map(|since| since.as_millis() as i64)
                .unwrap_or_default(),
            version: fetched.version.clone(),
        })
        .collect()
}

/// Keeps the contents of the remote files that a run read in its store, where it does not
/// have them yet.
pub async fn keep(store: &Store, fetched: &[Fetched]) -> Result<(), String> {
    for file in fetched {
        let digest: task_model::Digest = (file.digest.parse()).map_err(|_| {
            format!(
                "the digest {} of {} is not valid",
                file.digest, file.address
            )
        })?;
        if store
            .has_content(&digest)
            .await
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        let (kept, _) = (store.put_file(&file.copy).await).map_err(|error| error.to_string())?;
        if kept != digest {
            return Err(format!(
                "the copy of {} changed after the run read it",
                file.address
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::*;

    fn fetched(address: &str) -> Fetched {
        Fetched {
            address: address.into(),
            copy: PathBuf::from("/downloads/x"),
            digest: "sha256:ab".into(),
            size: 3,
            time: SystemTime::UNIX_EPOCH + Duration::from_millis(1500),
            version: Some("\"1\"".into()),
        }
    }

    #[test]
    fn rows_keep_the_time_in_milliseconds() {
        let rows = rows(&[fetched("https://example.org/a.csv")]);
        assert_eq!(rows[0].fetched, 1500);
        assert_eq!(rows[0].version.as_deref(), Some("\"1\""));
    }
}
