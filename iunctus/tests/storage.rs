//! Storage providers of the config file in whole runs: a private web server whose token comes
//! from an environment variable, and whose secret shows nowhere, not in the output, the run
//! database, the RO-Crate, the files of the simulator or the cache. The ignored test reads the
//! inputs of a real run from one S3 server, keeps the store on another, as `just storage-test`
//! starts them, and publishes the results to a third provider, a folder.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

mod common;

/// The token of the private server, which only the environment of iunctus holds.
const TOKEN: &str = "token-of-the-test-4417";

/// A web server on 127.0.0.1 that serves `files` only to requests with the bearer token. In
/// the files, `__SERVER__` stands for the address of the server.
fn private_server(files: &[(&str, &str)]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    let files: HashMap<String, String> = files
        .iter()
        .map(|(path, body)| (path.to_string(), body.replace("__SERVER__", &address)))
        .collect();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let (mut first, mut authorized) = (String::new(), false);
            let _ = reader.read_line(&mut first);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                authorized |= line.trim() == format!("authorization: Bearer {TOKEN}")
                    || line.trim() == format!("Authorization: Bearer {TOKEN}");
            }
            let mut parts = first.split_whitespace();
            let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let (status, body) = match files.get(path) {
                _ if !authorized => ("401 Unauthorized", String::new()),
                Some(body) => ("200 OK", body.clone()),
                None => ("404 Not Found", String::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if method != "HEAD" {
                let _ = stream.write_all(body.as_bytes());
            }
        }
    });
    address
}

/// Runs the binary in `directory` with its own cache and config folders, and the environment
/// `environment`; the result has the exit code and the standard output and error.
fn iunctus(
    directory: &Path,
    cache: &str,
    environment: &[(&str, &str)],
    arguments: &[&str],
) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join(cache))
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .envs(environment.iter().copied())
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// The files below a folder that hold `text`.
fn files_with(folder: &Path, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_with(&path, text));
        } else if let Ok(bytes) = std::fs::read(&path)
            && bytes
                .windows(text.len())
                .any(|window| window == text.as_bytes())
        {
            found.push(path.display().to_string());
        }
    }
    found
}

const SCRIPT: &str = r#"
params.samples = '__SERVER__/private/samples.csv'

process COUNT {
    input:
    tuple val(id), path(reads)
    output:
    path "${id}.count"
    script:
    """
    wc -l < ${reads} > ${id}.count
    """
}

workflow {
    channel.of(file(params.samples)).splitCsv(header: true).map { row -> tuple(row.id, file(row.reads)) } | COUNT
}
"#;

#[test]
fn secrets_never_show() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    // The sample sheet names the reads on the same server.
    let server = private_server(&[
        (
            "/private/samples.csv",
            "id,reads\na,__SERVER__/private/a.fq\n",
        ),
        ("/private/a.fq", "@r\nACGT\n+\nIIII\n"),
    ]);
    std::fs::write(path.join("main.nf"), SCRIPT.replace("__SERVER__", &server)).expect("writes");
    let config = format!(
        "[storage.private]\ntype = \"https\"\nprefixes = [\"{server}/private/\"]\n\
         token = {{ environment = \"IUNCTUS_TEST_TOKEN\" }}\n"
    );
    std::fs::write(path.join("iunctus.toml"), config).expect("writes the config file");

    let right = [("IUNCTUS_TEST_TOKEN", TOKEN)];
    let run = ["run", "main.nf", "-iunctus-config", "iunctus.toml"];
    let (code, output) = iunctus(path, "cache", &right, &run);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[simulated] COUNT (1)"), "{output}");
    let (code, listed) = iunctus(path, "cache", &[], &["runs"]);
    assert_eq!(code, 0, "{listed}");
    let id = listed
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .expect("a run id")
        .to_owned();
    let (code, exported) = iunctus(path, "cache", &[], &["export", &id, "-output", "crate"]);
    assert_eq!(code, 0, "{exported}");

    // A wrong token is an error that names the file, never a token.
    let wrong = "wrong-token-of-the-test-9981";
    let (code, refused) = iunctus(path, "other", &[("IUNCTUS_TEST_TOKEN", wrong)], &run);
    assert_eq!(code, 1, "{refused}");
    assert!(
        refused.contains(&format!(
            "{server}/private/samples.csv (storage provider `private`)"
        )),
        "{refused}"
    );
    assert!(
        refused.contains("the server refused access (401 Unauthorized)"),
        "{refused}"
    );
    // A config file whose variable is not set says so.
    let (code, unset) = iunctus(path, "other", &[], &run);
    assert_eq!(code, 1, "{unset}");
    assert!(
        unset.contains(
            "the environment variable IUNCTUS_TEST_TOKEN, which holds this secret, is not set"
        ),
        "{unset}"
    );

    for secret in [TOKEN, wrong] {
        for text in [&output, &listed, &exported, &refused, &unset] {
            assert!(!text.contains(secret), "{text}");
        }
        // The run database, the crate, the task documents of the simulator and the downloads.
        assert_eq!(files_with(path, secret), Vec::<String>::new());
    }
    assert!(path.join("crate/ro-crate-metadata.json").is_file());
}

const CWL_WORKFLOW: &str = "cwlVersion: v1.2
class: Workflow
inputs: []
outputs: []
steps:
  hello:
    run: __SERVER__/private/tool.cwl
    in: {}
    out: []
";

const CWL_TOOL: &str = "cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs: []
outputs: []
";

#[test]
fn loads_provider_documents() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    let server = private_server(&[("/private/tool.cwl", CWL_TOOL)]);
    std::fs::write(
        path.join("wf.cwl"),
        CWL_WORKFLOW.replace("__SERVER__", &server),
    )
    .expect("writes the workflow");
    let config = format!(
        "[storage.private]\ntype = \"https\"\nprefixes = [\"{server}/private/\"]\n\
         token = {{ environment = \"IUNCTUS_TEST_TOKEN\" }}\n"
    );
    std::fs::write(path.join("iunctus.toml"), config).expect("writes the config file");
    let graph = ["graph", "wf.cwl", "-iunctus-config", "iunctus.toml"];

    let (code, output) = iunctus(path, "cache", &[("IUNCTUS_TEST_TOKEN", TOKEN)], &graph);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("hello"), "{output}");
    // Without the token, the private server refuses the document, and the error names the
    // provider.
    let wrong = "wrong-token-of-the-test-9981";
    let (code, refused) = iunctus(path, "other", &[("IUNCTUS_TEST_TOKEN", wrong)], &graph);
    assert_eq!(code, 1, "{refused}");
    assert!(
        refused.contains("the server refused access (401 Unauthorized)"),
        "{refused}"
    );
    assert!(
        !refused.contains(wrong) && !refused.contains(TOKEN),
        "{refused}"
    );
}

/// An environment variable that `just storage-test` sets.
fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set by `just storage-test`"))
}

const REAL_SCRIPT: &str = r#"
process COUNT {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(reads)
    output:
    tuple val(id), path("${id}.count")
    script:
    """
    wc -l < ${reads} > ${id}.count
    """
}

process TOTAL {
    container 'debian:stable-slim'
    publishDir 's3://results/run', mode: 'copy'
    input:
    path counts
    output:
    path "total.txt"
    script:
    """
    cat ${counts} > total.txt
    """
}

workflow {
    samples = channel.of(file('s3://inputs/run/samples.csv')).splitCsv(header: true)
    counted = COUNT(samples.map { row -> tuple(row.id, file(row.reads)) })
    TOTAL(counted.map { id, count -> count }.collect())
}
"#;

#[test]
#[ignore = "needs Docker, the debian:stable-slim image and two S3 servers: just storage-test"]
fn separates_s3_providers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    let config = format!(
        "[store]\nlocation = \"s3://cache/iunctus\"\n\
         [storage.inputs]\ntype = \"s3\"\nprefixes = [\"s3://inputs/\"]\nendpoint = \"{}\"\n\
         access_key_id = {{ environment = \"IUNCTUS_TEST_INPUTS_KEY\" }}\n\
         secret_access_key = {{ environment = \"IUNCTUS_TEST_INPUTS_SECRET\" }}\n\
         [storage.store]\ntype = \"s3\"\nprefixes = [\"s3://cache/\"]\nendpoint = \"{}\"\n\
         access_key_id = {{ environment = \"IUNCTUS_TEST_STORE_KEY\" }}\n\
         secret_access_key = {{ environment = \"IUNCTUS_TEST_STORE_SECRET\" }}\n\
         [storage.results]\ntype = \"folder\"\nprefixes = [\"s3://results/\"]\npath = \"{}\"\n",
        variable("IUNCTUS_TEST_INPUTS_ENDPOINT"),
        variable("IUNCTUS_TEST_STORE_ENDPOINT"),
        path.join("results").display(),
    );
    std::fs::write(path.join("iunctus.toml"), &config).expect("writes the config file");
    // The inputs go to the server of the inputs first.
    let parsed = config_file::ConfigFile::parse(&config, Path::new("iunctus.toml")).expect("TOML");
    let storage = storage::Storage::from_config(&parsed).expect("valid settings");
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let upload = |name: &str, text: &str| {
        let local = path.join(name);
        std::fs::write(&local, text).expect("writes an input");
        let target = format!("s3://inputs/run/{name}");
        runtime
            .block_on(storage.upload(&local, &target))
            .expect("an upload");
    };
    upload("a.fq", "@r\nACGT\n+\nIIII\n");
    upload("b.fq", "@r\nAC\n+\nII\n@s\nGT\n+\nII\n");
    upload(
        "samples.csv",
        "id,reads\na,s3://inputs/run/a.fq\nb,s3://inputs/run/b.fq\n",
    );
    std::fs::write(path.join("main.nf"), REAL_SCRIPT).expect("writes the script");
    let environment: Vec<(String, String)> = [
        "IUNCTUS_TEST_INPUTS_KEY",
        "IUNCTUS_TEST_INPUTS_SECRET",
        "IUNCTUS_TEST_STORE_KEY",
        "IUNCTUS_TEST_STORE_SECRET",
    ]
    .iter()
    .map(|name| (name.to_string(), variable(name)))
    .collect();
    let environment: Vec<(&str, &str)> = (environment.iter())
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let run = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-iunctus-config",
        "iunctus.toml",
    ];
    let (code, first) = iunctus(path, "cache", &environment, &run);
    assert_eq!(code, 0, "{first}");
    assert!(first.contains("[done] TOTAL"), "{first}");
    // The results went to the third provider.
    let total = std::fs::read_to_string(path.join("results/run/total.txt")).expect("results");
    let mut counts: Vec<&str> = total.lines().map(str::trim).collect();
    counts.sort();
    assert_eq!(counts, ["4", "8"]);
    // The store lives on the second server: it holds the contents of the outputs.
    let stored = runtime
        .block_on(storage.list("s3://cache/iunctus/"))
        .expect("a listing of the store");
    let names: Vec<&str> = stored.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"cas") && names.contains(&"ac"), "{names:?}");
    // A second run takes every task from the store.
    let (code, second) = iunctus(path, "cache", &environment, &run);
    assert_eq!(code, 0, "{second}");
    assert!(second.contains("[cached] TOTAL"), "{second}");
    assert!(!second.contains("[done]"), "{second}");
    for (_, secret) in environment
        .iter()
        .filter(|(name, _)| name.ends_with("SECRET"))
    {
        for text in [&first, &second] {
            assert!(!text.contains(secret), "{text}");
        }
        assert_eq!(files_with(path, secret), Vec::<String>::new());
    }
}
