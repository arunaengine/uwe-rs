//! nf-tower 1.28.4: reports a run and its tasks to the trace API of Seqera Platform, with the
//! `dag` of 1.29 and the retried timeouts of 1.30.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod client;
mod limits;
mod metrics;
mod reports;
mod settings;
mod trace;
mod worker;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use nextflow_config::Config;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, Event, Observer};
use settings::Settings;
use worker::Worker;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["tower"],
    observer: Some(activate),
    ..Plugin::new("nf-tower", Kind::Observer, State::Partial)
};

/// How often the worker looks for task records to send.
const TICK: Duration = Duration::from_millis(100);
/// How long the end of the run waits for the last requests, as `terminateTimeout` does.
const WAIT: Duration = Duration::from_secs(120);

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let Some(settings) = Settings::read(config, &|name| std::env::var(name).ok())? else {
        return Ok(None);
    };
    let shared = Arc::new(Shared::default());
    let limit = settings.api.timeout * 2;
    let mut worker = Worker::new(settings, shared.clone());
    let background = Background::ticking("nf-tower", WAIT, TICK, move |event| {
        worker.handle(event);
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(Tower {
        background,
        shared,
        limit,
        sent: 0,
    })))
}

/// The observer for a config without a `plugins` line for nf-tower, which Nextflow loads by
/// itself when `tower.enabled` is true or Seqera Platform launched the run.
pub(crate) fn implied(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    activate(config)
}

/// The run observer: hands the events to the worker and lets a failed report fail the run.
struct Tower {
    background: Background,
    shared: Arc<Shared>,
    /// How long the end of a run waits for the worker to send what it holds.
    limit: Duration,
    /// The events handed to the worker.
    sent: u64,
}

/// What the worker tells the run.
#[derive(Default)]
pub(super) struct Shared {
    status: Mutex<Status>,
    changed: Condvar,
}

#[derive(Default)]
pub(super) struct Status {
    /// The run was created and began on the Platform, or that failed.
    ready: bool,
    /// The message that fails the run, as Nextflow aborts it.
    failure: Option<String>,
    /// The events that the worker handled.
    heard: u64,
    /// The run asks the worker to send the records it holds once it heard this many events.
    flush: Option<u64>,
    flushed: bool,
}

impl Shared {
    pub(super) fn update(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut status) = self.status.lock() {
            change(&mut status);
            self.changed.notify_all();
        }
    }
}

impl Observer for Tower {
    fn notify(&mut self, event: &Event) {
        self.sent += 1;
        self.background.notify(event);
    }

    /// A report that failed before the end fails the run, as it aborts Nextflow's session.
    fn check(&mut self) -> Result<(), String> {
        let Ok(mut status) = self.shared.status.lock() else {
            return Ok(());
        };
        status.flush = Some(self.sent);
        let waiting =
            |status: &mut Status| status.failure.is_none() && !(status.ready && status.flushed);
        match self
            .shared
            .changed
            .wait_timeout_while(status, self.limit, waiting)
        {
            Ok((status, wait)) => match status.failure.clone() {
                Some(failure) => Err(failure),
                None if wait.timed_out() => Err(format!(
                    "Seqera Platform did not take the reports of the run within {} ms",
                    self.limit.as_millis()
                )),
                None => Ok(()),
            },
            Err(_) => Ok(()),
        }
    }
}
