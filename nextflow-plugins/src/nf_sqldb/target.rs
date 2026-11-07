//! Maps JDBC connection URLs onto the database clients of uwe, and refuses the others.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_evaluator::interpreter::Host;

use super::athena::Athena;
use super::databricks::Databricks;
use super::server::{Engine, Server, Tls};
use super::trino::Trino;
use super::{Connection, Source};

/// What uwe connects to for a JDBC URL.
#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    /// A SQLite file, an in-memory database for `None`, or a `file:` URI.
    Sqlite(Option<String>),
    Server(Server),
    Trino(Trino),
    Athena(Athena),
    Databricks(Databricks),
}

/// The URL forms that uwe connects to, for the message of a refused URL.
const SUPPORTED: &str = "jdbc:sqlite:, jdbc:postgresql:, jdbc:mysql:, jdbc:mariadb:, jdbc:trino:, \
                         jdbc:awsathena: and jdbc:databricks:";

/// Opens a connection to the database of a source.
pub(crate) fn connect(source: &Source, host: &dyn Host) -> Result<Box<dyn Connection>, String> {
    let variable = |name: &str| host.environment(name);
    match target(source, &host.launch_dir(), &variable)? {
        Target::Sqlite(place) => Ok(Box::new(super::sqlite::open(place.as_deref())?)),
        Target::Server(server) => Ok(Box::new(super::server::open(server)?)),
        Target::Trino(trino) => Ok(Box::new(trino)),
        Target::Athena(athena) => Ok(Box::new(athena)),
        Target::Databricks(databricks) => Ok(Box::new(databricks)),
    }
}

/// Whether uwe refuses the URL, rather than failing to connect. A `jdbc:starburst:` URL fails
/// to connect, as it does in nf-trino.
pub(crate) fn refused(url: &str) -> bool {
    let word = url.split(':').nth(1).unwrap_or_default();
    let known = [
        "sqlite",
        "postgresql",
        "mysql",
        "mariadb",
        "trino",
        "starburst",
        "awsathena",
        "athena",
        "databricks",
    ];
    !known.contains(&word)
}

/// The target of a source's URL; the user and password of the config win over those of the URL.
pub(crate) fn target(
    source: &Source,
    launch: &str,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Result<Target, String> {
    let url = &source.url;
    let rest = url.strip_prefix("jdbc:").unwrap_or(url);
    let (word, address) = rest.split_once(':').unwrap_or((rest, ""));
    match word {
        "sqlite" => Ok(Target::Sqlite(sqlite_place(address, launch))),
        "postgresql" => server(Engine::Postgres, address, source),
        "mysql" | "mariadb" => server(Engine::Mysql, address, source),
        "trino" => {
            // The fallback user `sa` gives way to a user of the URL.
            let user =
                (source.user.clone()).filter(|user| user != "sa" || !address.contains("user="));
            let trino = super::trino::coordinator(address, user, source.password.clone());
            trino.map(Target::Trino)
        }
        "awsathena" | "athena" => {
            let keys = source.password.as_ref().and(source.user.clone());
            let athena = super::athena::workgroup(address, keys, source.password.clone(), variable);
            athena.map(Target::Athena)
        }
        "databricks" => {
            let databricks = super::databricks::warehouse(address, source.password.clone());
            databricks.map(Target::Databricks)
        }
        "starburst" => Err(format!("No suitable driver found for {url}")),
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

/// A PostgreSQL or MySQL server of `//host[:port]/database[?key=value&...]`, or of the short
/// forms `database` and `/` on the local host.
fn server(engine: Engine, address: &str, source: &Source) -> Result<Target, String> {
    let (place, query) = address.split_once('?').unwrap_or((address, ""));
    let parameters: Vec<(String, String)> = (query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key.to_owned(), value.to_owned())
        })
        .collect();
    let parameter = |name: &str| {
        let found = parameters
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name));
        found.map(|(_, value)| value.clone())
    };
    let (authority, database) = match place.strip_prefix("//") {
        Some(rest) => rest.split_once('/').unwrap_or((rest, "")),
        None => ("", place.trim_start_matches('/')),
    };
    // The first host of a list of fail-over hosts.
    let authority = authority.split(',').next().unwrap_or_default();
    // An IPv6 host is in brackets, which a port follows after a colon.
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (host, after) = rest.split_once(']').unwrap_or((rest, ""));
            (host, after.strip_prefix(':'))
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(port) => Some(
            port.parse::<u16>()
                .map_err(|_| format!("the port of the JDBC URL {} is not a number", source.url))?,
        ),
        None => None,
    };
    let host = match host {
        "" => "localhost".to_owned(),
        host => host.to_owned(),
    };
    let default_port = match engine {
        Engine::Postgres => 5432,
        Engine::Mysql => 3306,
    };
    let tls = match engine {
        Engine::Postgres => match parameter("sslmode").as_deref() {
            Some("disable") => Tls::Disable,
            Some("allow" | "prefer") => Tls::Prefer,
            Some("require") => Tls::Require,
            Some("verify-ca" | "verify-full") => Tls::Verify,
            _ if parameter("ssl").as_deref() == Some("true") => Tls::Verify,
            _ => Tls::Prefer,
        },
        Engine::Mysql => match parameter("sslMode").map(|mode| mode.to_ascii_uppercase()) {
            Some(mode) if mode == "DISABLED" || mode == "DISABLE" => Tls::Disable,
            Some(mode) if mode == "REQUIRED" || mode == "TRUST" => Tls::Require,
            Some(mode) if mode.starts_with("VERIFY") => Tls::Verify,
            Some(_) => Tls::Prefer,
            // Connector/J's mapping of its older flags, where `useSSL` defaults to true.
            None => {
                let flag =
                    |name: &str| parameter(name).map(|value| value.eq_ignore_ascii_case("true"));
                match (
                    flag("useSSL"),
                    flag("requireSSL"),
                    flag("verifyServerCertificate"),
                ) {
                    (Some(false), _, _) => Tls::Disable,
                    (_, _, Some(true)) => Tls::Verify,
                    (_, Some(true), _) => Tls::Require,
                    _ => Tls::Prefer,
                }
            }
        },
    };
    let user = (source
        .user
        .clone()
        .filter(|user| user != "sa" || parameter("user").is_none()))
    .or_else(|| parameter("user"));
    Ok(Target::Server(Server {
        engine,
        host,
        port: port.unwrap_or(default_port),
        database: Some(database.to_owned()).filter(|name| !name.is_empty()),
        user,
        password: source.password.clone().or_else(|| parameter("password")),
        tls,
    }))
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
    fn maps_server_urls() {
        let found = target(
            &source(
                "jdbc:postgresql://db.example:6543/lab?user=u&sslmode=require",
                None,
            ),
            "/",
            &|_| None,
        );
        let Ok(Target::Server(server)) = found else {
            panic!("a server: {found:?}")
        };
        assert_eq!(
            (
                server.host.as_str(),
                server.port,
                server.database.as_deref()
            ),
            ("db.example", 6543, Some("lab"))
        );
        assert_eq!(
            (server.user.as_deref(), server.tls),
            (Some("u"), Tls::Require)
        );
        let found = target(&source("jdbc:mysql://[::1]/x", Some("sa")), "/", &|_| None);
        let Ok(Target::Server(server)) = found else {
            panic!("a server: {found:?}")
        };
        assert_eq!((server.host.as_str(), server.port), ("::1", 3306));
        assert_eq!(server.user.as_deref(), Some("sa"));
        let found = target(&source("jdbc:postgresql:lab", Some("me")), "/", &|_| None);
        let Ok(Target::Server(server)) = found else {
            panic!("a server: {found:?}")
        };
        assert_eq!((server.host.as_str(), server.port), ("localhost", 5432));
        assert_eq!(server.user.as_deref(), Some("me"));
    }

    #[test]
    fn maps_legacy_tls() {
        let tls = |query: &str| match target(
            &source(&format!("jdbc:mysql://h/x?{query}"), None),
            "/",
            &|_| None,
        ) {
            Ok(Target::Server(server)) => server.tls,
            other => panic!("a server: {other:?}"),
        };
        let strict = "useSSL=true&requireSSL=true&verifyServerCertificate=true";
        assert_eq!(tls(strict), Tls::Verify);
        assert_eq!(tls("useSSL=true&requireSSL=true"), Tls::Require);
        assert_eq!(tls("useSSL=true"), Tls::Prefer);
        assert_eq!(tls("useSSL=false&requireSSL=true"), Tls::Disable);
        assert_eq!(tls(&format!("{strict}&sslMode=PREFERRED")), Tls::Prefer);
    }

    #[test]
    fn refuses_java_drivers() {
        let place = |url: &str| target(&source(url, None), "/launch", &|_| None);
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
