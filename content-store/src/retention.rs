//! Rules of garbage collection in shared stores: the grace window, the epoch that fences each
//! collection, and the hook of hosts that delete objects by retention rules of their own.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, SystemTime};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::{Object, Store, StoreError};

/// The shortest grace window of a collection in a shared store: a run of another machine may
/// write an object before its lease shows, so recent objects always stay this long.
pub const LEAST_GRACE: Duration = Duration::from_secs(60 * 60);

/// Where a shared store keeps the epoch of the last collection.
pub const EPOCH: &str = "maintenance/epoch.json";

/// How a collection in a shared store keeps what runs may still need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcPolicy {
    grace: Duration,
    /// How many objects one batch of the sweep removes before the lease and the epoch are
    /// checked again.
    pub batch: usize,
}

impl GcPolicy {
    /// A policy that keeps every object written within `grace`, at least [`LEAST_GRACE`].
    pub fn new(grace: Duration) -> Result<GcPolicy, StoreError> {
        if grace < LEAST_GRACE {
            return Err(StoreError::Location(format!(
                "the grace window of a shared store must be at least {} minutes",
                LEAST_GRACE.as_secs() / 60
            )));
        }
        Ok(GcPolicy { grace, batch: 64 })
    }

    pub fn grace(&self) -> Duration {
        self.grace
    }

    /// The time before which an object must have been written to go, for a collection that
    /// began at `now`.
    pub fn cutoff(&self, now: SystemTime) -> SystemTime {
        now.checked_sub(self.grace)
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    /// Whether an object is old enough to go: its write time is known and before the cutoff.
    pub fn expired(&self, object: &Object, now: SystemTime) -> bool {
        object
            .modified
            .is_some_and(|modified| modified < self.cutoff(now))
    }
}

/// The generation of collections in a store: each collection takes the next number, and its
/// sweep stops when another collection took one after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Epoch {
    pub number: u64,
    /// Who began the collection of this epoch, like the host and process.
    pub holder: String,
}

/// Deletes or expires objects of a shared store for uwe, for hosts whose objects follow
/// retention rules of their own, like the lifecycle rules of Aruna or of an object store.
pub trait RetentionDelegate: Send + Sync {
    /// Hands these objects of `store` to the retention of the host; gives how many it took.
    fn release<'a>(
        &'a self,
        store: &'a Store,
        objects: &'a [Object],
    ) -> BoxFuture<'a, Result<usize, StoreError>>;
}

impl Store {
    /// The epoch of the last collection, `None` before the first.
    pub async fn epoch(&self) -> Result<Option<Epoch>, StoreError> {
        match self.get_object(EPOCH).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| StoreError::Location(format!("{EPOCH}: {error}"))),
            Err(error) if error.missing() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Takes the next epoch for a collection that `holder` begins. The caller holds the
    /// maintenance lease, so that no other collection takes it at the same moment.
    pub async fn advance_epoch(&self, holder: &str) -> Result<Epoch, StoreError> {
        let number = self.epoch().await?.map_or(1, |epoch| epoch.number + 1);
        let epoch = Epoch {
            number,
            holder: holder.to_owned(),
        };
        let bytes = serde_json::to_vec(&epoch).map_err(std::io::Error::other)?;
        self.replace(EPOCH, &bytes).await?;
        Ok(epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advances_epochs() {
        let store = Store::memory().unwrap();
        assert_eq!(store.epoch().await.unwrap(), None);
        assert_eq!(store.advance_epoch("a").await.unwrap().number, 1);
        let second = store.advance_epoch("b").await.unwrap();
        assert_eq!(store.epoch().await.unwrap(), Some(second));
    }

    #[test]
    fn keeps_recent_objects() {
        assert!(
            GcPolicy::new(Duration::from_secs(60)).is_err(),
            "below the floor"
        );
        let policy = GcPolicy::new(Duration::from_secs(2 * 3600)).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        let object = |age: Option<u64>| Object {
            location: "cas/x".into(),
            size: 1,
            modified: age.map(|age| now - Duration::from_secs(age)),
            file: None,
            folder: false,
        };
        assert!(policy.expired(&object(Some(3 * 3600)), now));
        assert!(!policy.expired(&object(Some(3600)), now));
        assert!(
            !policy.expired(&object(None), now),
            "an unknown write time stays"
        );
    }
}
