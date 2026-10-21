//! Checks cached task results without executing tasks or populating storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use content_store::Store;
use task_model::ReuseScope;
use workflow_graph::{Outcome, Reuse, Task};

use crate::entry::{self, Stale};
use crate::executor::outcome_of;
use crate::inputs::{Inputs, Mode, Snapshots};
use crate::request::prepare;
use crate::{RunnerError, Settings};

/// Looks up the results of tasks in a store.
#[derive(Clone, Debug)]
pub struct Lookup {
    store: Store,
    settings: Settings,
}

impl Lookup {
    pub fn new(store: Store, settings: Settings) -> Lookup {
        Lookup { store, settings }
    }

    /// The outcome that a real run would take from the cache for a task whose image is pinned
    /// to `image`, with the stored files.
    pub async fn find(
        &self,
        task: &Task,
        image: Option<String>,
    ) -> Result<Option<Outcome>, RunnerError> {
        if task.reuse != Reuse::Allowed {
            return Ok(None);
        }
        let store = self.store.without_read_cache();
        let inputs = Inputs {
            store: &store,
            settings: &self.settings,
            mode: Mode::Look,
            snapshots: Snapshots::new(&self.settings, "lookup"),
        };
        let Ok(prepared) = prepare(&inputs, task, image.clone()).await else {
            return Ok(None);
        };
        let spec = &prepared.request.spec;
        if prepared.request.scope != ReuseScope::Shared || !prepared.reuse {
            return Ok(None);
        }
        if let Some(image) = &spec.image {
            let tool = crate::check::tool(&image.reference, &prepared.step);
            if store.unreproducible(&tool).await?.is_some() {
                return Ok(None);
            }
        }
        let key = spec.cache_key()?;
        let found = entry::stored(&store, &spec.namespace, &key, Stale::Keep).await?;
        match found {
            Some(entry) => {
                let outcome = outcome_of(&store, task, 1, entry.result(), image).await?;
                Ok(Some(outcome))
            }
            None => Ok(None),
        }
    }
}
