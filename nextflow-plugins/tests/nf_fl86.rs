//! nf-fl86 against what nf-fl86 0.6.4 wrote and sent with Nextflow 26.04.6: the `pipeline_info`
//! files, the Notion requests to a local fake, the start and end lines and the folder guard.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A fake Notion tracker that answers 200; the request lines and headers it heard.
fn tracker() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a free port");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                lines.push(line.trim().to_owned());
            }
            log.lock().expect("the log").push(lines.join("\n"));
            let _ = (&stream)
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (port, heard)
}

#[test]
fn records_and_notifies() {
    let (port, heard) = tracker();
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_fl86");
    common::copy(&fixtures, folder, &["samples.csv", "benchling.json"]);
    let script = "process P {\n  input: val s\n  output: val s\n  script: \"echo ${s}\"\n}\n\
                  workflow {\n  P(channel.of('s1'))\n}\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-fl86@0.6.4' }}\nmanifest.name = 'fl86-probe'\nworkDir = '{0}/work'\n\
         params {{ outdir = '{0}/out'; sample_sheet = '{0}/samples.csv'; \
         benchling = '{0}/benchling.json'; mproj = 'PRJ1'; nested = [a: 1]; zeta = 'z' }}\n\
         notion {{\n  beginning_endpoint = \
         'http://127.0.0.1:{port}/begin?ngsrun={{NGSRUN}}&seqeraPlatformUrl={{seqeraPlatformUrl}}'\n  \
         complete_endpoint = 'http://127.0.0.1:{port}/done?ngsrun=${{NGSRUN}}'\n}}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "probe_NGSRUN123_x");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    assert!(
        printed.starts_with("Pipeline is starting (nf-fl86)! 🚀\n"),
        "{printed}"
    );
    assert!(
        printed.ends_with("Pipeline complete! (nf-fl86)👋\n"),
        "{printed}"
    );
    let info = folder.join("out/pipeline_info");
    let read = |name: &str| std::fs::read_to_string(info.join(name)).unwrap_or_default();
    assert_eq!(
        read("sample_sheet.csv"),
        std::fs::read_to_string(fixtures.join("samples.csv")).expect("CSV")
    );
    assert_eq!(
        read("benchling.json"),
        std::fs::read_to_string(fixtures.join("benchling.json")).expect("JSON")
    );
    let params = read("params.json");
    let probe = folder.display().to_string();
    let expected = "{\n    \"benchling\": \"<probe>/benchling.json\",\n    \"mproj\": \"PRJ1\",\n    \
                    \"nested\": {\n        \"a\": 1\n    },\n    \"outdir\": \"<probe>/out\",\n    \
                    \"sample_sheet\": \"<probe>/samples.csv\",\n    \"zeta\": \"z\"\n}";
    assert_eq!(params, expected.replace("<probe>", &probe));
    let written: serde_json::Value = serde_json::from_str(&read("workflow.json")).expect("JSON");
    let recorded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures.join("workflow.json")).expect("JSON"),
    )
    .expect("JSON");
    assert_eq!(written["manifest"], recorded["manifest"]);
    assert_eq!(written["runName"], "probe_NGSRUN123_x");
    assert_eq!(written["nextflowVersion"], "26.04.6");
    assert!(folder.join("work/pipeline.complete").is_file());
    let heard = heard.lock().expect("the log").clone();
    assert_eq!(heard.len(), 2, "{heard:?}");
    assert!(
        heard[0].starts_with("POST /begin?ngsrun=NGSRUN123&seqeraPlatformUrl= HTTP/1.1"),
        "{heard:?}"
    );
    assert!(
        heard[1].starts_with("POST /done?ngsrun=$NGSRUN123 HTTP/1.1"),
        "{heard:?}"
    );
    assert!(
        heard[0]
            .to_ascii_lowercase()
            .contains("x-http-method-override: patch"),
        "{heard:?}"
    );
    let (execution, printed) = common::run(folder, "again");
    let error = execution
        .error
        .map(|error| error.to_string())
        .unwrap_or_default();
    let guard = format!(
        "🚨 Terminating execution! Output directory already exists: {probe}/out (must be cleared \
         to re-run workflow) 🚨"
    );
    assert!(error.contains(&guard), "{error}\n{printed}");
}
