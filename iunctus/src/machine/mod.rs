//! `iunctus check-machine`: what is ready on this machine for real runs, and what is missing
//! with what to install or set: the config file and its secrets, the container engine, the
//! store and its free space, and the run database.
//!
//! The check asks no server outside this machine, except the store at an address that the
//! config file or the command line names, once.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod config;
mod local;

use std::fmt;

use crate::Failure;
use crate::arguments::Arguments;
use crate::failure::{SETUP, without_error_numbers};

/// How a part of the machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Ready,
    /// Missing or broken: a run that needs it stops.
    Missing,
    /// Not set up, and not needed unless a run asks for it.
    NotSetUp,
}

impl fmt::Display for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            State::Ready => "ready",
            State::Missing => "missing",
            State::NotSetUp => "not set up",
        })
    }
}

/// One part of the machine and what the check found.
struct Found {
    state: State,
    part: String,
    text: String,
}

/// What the check found, in order.
#[derive(Default)]
pub(super) struct Report {
    found: Vec<Found>,
}

impl Report {
    pub(super) fn add(&mut self, state: State, part: impl Into<String>, text: impl Into<String>) {
        self.found.push(Found {
            state,
            part: part.into(),
            text: without_error_numbers(&text.into()),
        });
    }

    fn text(&self) -> String {
        let mut text = String::new();
        for found in &self.found {
            text.push_str(&format!(
                "{:<11}{}: {}\n",
                found.state.to_string(),
                found.part,
                found.text
            ));
        }
        let missing = (self.found.iter())
            .filter(|found| found.state == State::Missing)
            .count();
        text.push_str(&match missing {
            0 => "This machine is ready for real runs.\n".to_owned(),
            1 => "1 thing is missing for real runs; the line that starts with `missing` says what \
                  to do.\n"
                .to_owned(),
            count => format!(
                "{count} things are missing for real runs; the lines that start with `missing` \
                 say what to do.\n"
            ),
        });
        text
    }
}

/// `iunctus check-machine`: prints what is ready and what is missing, and exits with 3 when
/// something is missing.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let mut report = Report::default();
    let setup = config::config(arguments, &mut report);
    let runtime = workflow_graph::runtime().map_err(|error| Failure::setup(error.to_string()))?;
    local::engine(runtime, &setup, &mut report);
    local::store(runtime, &setup, arguments, &mut report);
    local::database(arguments, &mut report);
    print!("{}", report.text());
    match report
        .found
        .iter()
        .any(|found| found.state == State::Missing)
    {
        true => Err(Failure::said(SETUP)),
        false => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_what_is_missing() {
        let mut report = Report::default();
        report.add(State::Ready, "store", "/s, 1.0 GB free");
        report.add(State::NotSetUp, "executor tes", "no TES server named");
        assert_eq!(
            report.text(),
            "ready      store: /s, 1.0 GB free\nnot set up executor tes: no TES server \
             named\nThis machine is ready for real runs.\n"
        );
        report.add(
            State::Missing,
            "container engine",
            "no container engine answers",
        );
        assert!(report.text().ends_with("1 thing is missing for real runs; the line that starts with `missing` says what to do.\n"));
    }
}
