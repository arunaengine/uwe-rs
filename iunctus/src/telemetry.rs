//! Writes and sends recorded run traces using configured OTLP HTTP settings.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::BufWriter;
use std::path::PathBuf;

use config_file::{ConfigError, ConfigFile, Secret};
use run_telemetry::{Request, SPANS_PER_REQUEST, Sent, traces_address};

use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, failure, runs};

/// `[telemetry]`: where the traces of runs go.
#[derive(Clone, Debug)]
pub struct Telemetry {
    /// `endpoint`: the base address of an OTLP HTTP endpoint, like `http://localhost:4318`;
    /// traces go to `/v1/traces` below it.
    pub endpoint: String,
    /// `headers`: headers of every request, each value a secret, like a token.
    pub headers: Vec<(String, Secret)>,
}

impl Telemetry {
    /// The `[telemetry]` section of the config files, if they have one.
    pub fn read(config: &ConfigFile) -> Result<Option<Telemetry>, ConfigError> {
        let Some(section) = config.section("telemetry")? else {
            return Ok(None);
        };
        section.only(&["endpoint", "headers"])?;
        let Some(endpoint) = section.text("endpoint")? else {
            return Err(section.missing(
                "endpoint",
                "the address of an OTLP HTTP endpoint, like http://localhost:4318",
            ));
        };
        let mut headers = Vec::new();
        if let Some(table) = section.table("headers")? {
            for name in table.keys() {
                if let Some(value) = table.secret(name)? {
                    headers.push((name.to_owned(), value));
                }
            }
        }
        Ok(Some(Telemetry { endpoint, headers }))
    }
}

/// `iunctus telemetry <run>`: writes the trace of the run to `-output`, by default
/// `<run>-traces.jsonl`, and sends it to the endpoint of the config file.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let Some(run) = arguments.target.as_ref().map(|run| run.to_string_lossy()) else {
        return Err(Failure::usage(
            "iunctus: name a run, like `iunctus telemetry 1a0f1cc4c10-3a2487`; `iunctus runs` \
             lists the runs",
        ));
    };
    let setup = Setup::load(arguments)?;
    let database = runs::open(arguments)?;
    super::run(&database, &run)?;
    let requests = traces(&database, &run)?;
    let output =
        (arguments.output.clone()).unwrap_or_else(|| PathBuf::from(format!("{run}-traces.jsonl")));
    let written = std::fs::File::create(&output)
        .map_err(run_telemetry::TelemetryError::from)
        .and_then(|file| run_telemetry::write_lines(&requests, BufWriter::new(file)));
    written.map_err(|error| {
        failure(format!(
            "iunctus: cannot write {}: {error}",
            output.display()
        ))
    })?;
    let spans: usize = requests.iter().map(|request| request.spans().count()).sum();
    println!(
        "wrote the trace of run {run} to {}: {spans} spans in OTLP JSON lines",
        output.display()
    );
    if let Some(telemetry) = &setup.telemetry {
        let sent = send(telemetry, &requests)?;
        println!("{}", said(telemetry, &sent));
    }
    Ok(())
}

impl Telemetry {
    /// Starts live export. Close the recorder before finishing the handle.
    pub fn start(
        &self,
        arguments: &Arguments,
        recorder: &mut run_database::Recorder,
    ) -> Option<run_telemetry::Live> {
        let telemetry = self;
        let live = (|| {
            let headers = telemetry
                .headers
                .iter()
                .map(|(name, secret)| secret.reveal().map(|value| (name.clone(), value)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let client = storage::http_client()?.clone();
            run_telemetry::Live::start(
                &runs::database_path(arguments),
                recorder,
                client,
                telemetry.endpoint.clone(),
                headers,
            )
            .map_err(|error| error.to_string())
        })();
        match live {
            Ok(live) => Some(live),
            Err(message) => {
                tracing::warn!("live telemetry could not start: {message}");
                None
            }
        }
    }
}

/// The trace of a run in export requests.
fn traces(database: &run_database::Database, run: &str) -> Result<Vec<Request>, Failure> {
    run_telemetry::requests(database, run, SPANS_PER_REQUEST)
        .map_err(|error| failure(format!("iunctus: {error}")))?
        .ok_or_else(|| failure(format!("iunctus: the run database has no run `{run}`")))
}

/// Sends export requests to the endpoint, with the headers of the config file, through the one
/// HTTP client of the process.
fn send(telemetry: &Telemetry, requests: &[Request]) -> Result<Sent, Failure> {
    let headers = (telemetry.headers.iter())
        .map(|(name, secret)| Ok((name.clone(), secret.reveal()?)))
        .collect::<Result<Vec<_>, ConfigError>>()
        .map_err(|error| failure(format!("iunctus: {error}")))?;
    let client =
        storage::http_client().map_err(|message| failure(format!("iunctus: {message}")))?;
    let runtime =
        workflow_graph::runtime().map_err(|error| failure(format!("iunctus: {error}")))?;
    let sending = run_telemetry::send(client, &telemetry.endpoint, &headers, requests);
    runtime
        .block_on(sending)
        .map_err(|error| failure(format!("iunctus: {error}")))
}

/// Where the trace went, in one line.
fn said(telemetry: &Telemetry, sent: &Sent) -> String {
    let address = traces_address(&telemetry.endpoint);
    match sent.rejected {
        0 => format!("sent the trace to {address}: {} spans", sent.spans),
        rejected => format!(
            "sent the trace to {address}: {} spans, of which it rejected {rejected}",
            sent.spans
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn read(text: &str) -> Result<Option<Telemetry>, String> {
        let config =
            ConfigFile::parse(text, Path::new("config.toml")).map_err(|e| e.to_string())?;
        Telemetry::read(&config).map_err(|error| error.to_string())
    }

    #[test]
    fn loads_telemetry_settings() {
        assert!(read("").expect("no section").is_none());
        let text = "[telemetry]\nendpoint = \"http://localhost:4318\"\nheaders = { authorization \
                    = \"Bearer t\" }\n";
        let telemetry = read(text).expect("valid").expect("a section");
        assert_eq!(telemetry.endpoint, "http://localhost:4318");
        let [(name, value)] = telemetry.headers.as_slice() else {
            panic!("one header");
        };
        assert_eq!(name, "authorization");
        assert_eq!(value.reveal().expect("its text"), "Bearer t");
        let error = read("[telemetry]\nheaders = {}\n").expect_err("no endpoint");
        assert!(error.contains("endpoint"), "{error}");
        let error = read("[telemetry]\nendpoint = \"x\"\nprotocol = \"grpc\"\n").expect_err("x");
        assert!(error.contains("protocol"), "{error}");
    }
}
