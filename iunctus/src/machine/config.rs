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
