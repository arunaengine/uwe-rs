//! Reports configured machine and executor prerequisites for real runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "machine/executors.rs"]
mod executors;
#[path = "machine/local.rs"]
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
    local::engine(runtime, &setup, arguments, &mut report);
    local::store(runtime, &setup, arguments, &mut report);
    local::database(arguments, &mut report);
    executors::tes_server(&setup.executors, arguments, &mut report);
    executors::slurm(&setup, arguments, &mut report);
    executors::kubernetes(runtime, &setup.executors, arguments, &mut report);
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
    fn counts_missing_requirements() {
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

mod config {
    //! The config files of the machine, their storage providers, and the secrets that they take
    //! from environment variables.
    // Copyright (c) 2026 The iunctus Contributors
    // SPDX-License-Identifier: MIT or Apache-2.0

    use config_file::{ConfigFile, Table};

    use super::{Report, State};
    use crate::arguments::Arguments;
    use crate::setup::Setup;

    /// The config files, their storage providers, and the secrets that they take from environment
    /// variables. A config file with a mistake counts as missing, and the rest of the check goes
    /// on without it.
    pub(super) fn config(arguments: &Arguments, report: &mut Report) -> Setup {
        let part = "config file";
        let config = match ConfigFile::load(arguments.config_file.as_deref()) {
            Ok(config) => config,
            Err(error) => {
                report.add(
                    State::Missing,
                    part,
                    format!("{error}; fix it or remove it"),
                );
                return Setup::default();
            }
        };
        let setup = match Setup::load(arguments) {
            Ok(setup) => setup,
            Err(failure) => {
                let message = failure.message.trim_start_matches("iunctus: ");
                report.add(State::Missing, part, format!("{message}; fix it"));
                return Setup::default();
            }
        };
        let files: Vec<String> = (config.files().iter())
            .map(|file| file.display().to_string())
            .collect();
        if files.is_empty() {
            let user = config_file::user_file()
                .map_or("~/.config/iunctus/config.toml".to_owned(), |file| {
                    file.display().to_string()
                });
            report.add(
                State::NotSetUp,
                part,
                format!(
                    "none, so public files need no provider and the store is a folder in the \
                     cache; {user} can name storage providers and executors"
                ),
            );
            return setup;
        }
        let providers = providers(&config);
        report.add(
            State::Ready,
            part,
            format!("{}; {providers}", files.join(", ")),
        );
        for (origin, variable) in secrets(config.root()) {
            match std::env::var(&variable) {
                Ok(value) if !value.is_empty() => {}
                _ => report.add(
                    State::Missing,
                    "secret",
                    format!(
                        "{origin}: the environment variable {variable}, which holds this secret, is \
                         not set; set it before a run"
                    ),
                ),
            }
        }
        setup
    }

    /// The storage providers of the config file in words, like `storage providers lab (s3) and
    /// portal (https)`.
    fn providers(config: &ConfigFile) -> String {
        let tables = (config.section("storage").ok().flatten())
            .and_then(|section| section.tables().ok())
            .unwrap_or_default();
        let names: Vec<String> = tables
            .iter()
            .map(|table| {
                let name = table.name().trim_start_matches("storage.");
                match table.text("type").ok().flatten() {
                    Some(kind) => format!("{name} ({kind})"),
                    None => name.to_owned(),
                }
            })
            .collect();
        match names.split_last() {
            None => "no storage providers".to_owned(),
            Some((one, [])) => format!("storage provider {one}"),
            Some((last, others)) => format!("storage providers {} and {last}", others.join(", ")),
        }
    }

    /// Every secret of a table and the tables in it that an environment variable holds, with where
    /// it was written: each value `{ environment = "NAME" }`.
    fn secrets(table: Table<'_>) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for key in table.keys() {
            let Ok(Some(inner)) = table.table(key) else {
                continue;
            };
            if inner.keys().eq(["environment"]) {
                if let Ok(Some(secret)) = table.secret(key)
                    && let Some(variable) = secret.variable()
                {
                    found.push((secret.origin().to_string(), variable.to_owned()));
                }
            } else {
                found.extend(secrets(inner));
            }
        }
        found
    }

    #[cfg(test)]
    mod tests {
        use std::path::Path;

        use super::*;

        #[test]
        fn finds_configured_secrets() {
            let text = "[storage.lab]\ntype = \"s3\"\nprefixes = [\"s3://lab/\"]\nsecret_access_key = \
                        { environment = \"LAB_SECRET\" }\n\n[storage.portal]\ntype = \"https\"\n\
                        prefixes = [\"https://x/\"]\nheaders = { X-Key = { environment = \"KEY\" } \
                        }\n";
            let config =
                ConfigFile::parse(text, Path::new("config.toml")).expect("a valid config file");
            let found = secrets(config.root());
            assert_eq!(
                found,
                [
                    ("config.toml:4".to_owned(), "LAB_SECRET".to_owned()),
                    ("config.toml:9".to_owned(), "KEY".to_owned())
                ]
            );
        }
    }
}
