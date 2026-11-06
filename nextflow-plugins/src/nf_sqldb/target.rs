//! Maps JDBC connection URLs onto the database clients of uwe, and refuses the others.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use super::{Connection, Source};

/// What uwe connects to for a JDBC URL.
#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    /// A SQLite file, an in-memory database for `None`, or a `file:` URI.
    Sqlite(Option<String>),
}

/// The URL forms that uwe connects to, for the message of a refused URL.
const SUPPORTED: &str = "jdbc:sqlite:";

/// Opens a connection to the database of a source.
pub(crate) fn connect(source: &Source, launch: &str) -> Result<Box<dyn Connection>, String> {
    match target(source, launch)? {
        Target::Sqlite(place) => Ok(Box::new(super::sqlite::open(place.as_deref())?)),
    }
}

/// Whether uwe refuses the URL, rather than failing to connect.
pub(crate) fn refused(url: &str) -> bool {
    let word = url.split(':').nth(1).unwrap_or_default();
    word != "sqlite"
}

/// The target of a source's URL; the user and password of the config win over those of the URL.
pub(crate) fn target(source: &Source, launch: &str) -> Result<Target, String> {
    let url = &source.url;
    let rest = url.strip_prefix("jdbc:").unwrap_or(url);
    let (word, address) = rest.split_once(':').unwrap_or((rest, ""));
    match word {
        "sqlite" => Ok(Target::Sqlite(sqlite_place(address, launch))),
        "h2" => Err(format!(
            "H2 is a Java database, which uwe cannot run: {url}. uwe connects to {SUPPORTED} \
             URLs; set sql.db.<name>.url to one of them"
        )),
        "duckdb" => Err(format!(
            "uwe does not run DuckDB databases yet: {url}. uwe connects to {SUPPORTED} URLs"
        )),
        _ => Err(format!(
            "uwe has no client for the JDBC URL {url}, which needs a Java driver. uwe connects \
             to {SUPPORTED} URLs"
        )),
    }
}

/// The file of a SQLite URL: relative paths start at the launch directory.
fn sqlite_place(address: &str, launch: &str) -> Option<String> {
    match address {
        "" | ":memory:" => None,
        uri if uri.starts_with("file:") => Some(uri.to_owned()),
        path if Path::new(path).is_absolute() => Some(path.to_owned()),
        path => Some(Path::new(launch).join(path).to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(url: &str, user: Option<&str>) -> Source {
        Source {
            name: "db".into(),
            url: url.into(),
            user: user.map(Into::into),
            password: None,
        }
    }

    #[test]
    fn refuses_java_drivers() {
        let place = |url: &str| target(&source(url, None), "/launch");
        assert_eq!(place("jdbc:sqlite:"), Ok(Target::Sqlite(None)));
        assert_eq!(place("jdbc:sqlite::memory:"), Ok(Target::Sqlite(None)));
        assert_eq!(
            place("jdbc:sqlite:a.db"),
            Ok(Target::Sqlite(Some("/launch/a.db".into())))
        );
        assert_eq!(
            place("jdbc:sqlite:/x/a.db"),
            Ok(Target::Sqlite(Some("/x/a.db".into())))
        );
        let refused = place("jdbc:h2:mem:").expect_err("refused");
        assert!(refused.starts_with("H2 is a Java database"), "{refused}");
        assert!(place("jdbc:duckdb:").is_err());
        assert!(place("jdbc:oracle:thin:@x").is_err());
    }
}
