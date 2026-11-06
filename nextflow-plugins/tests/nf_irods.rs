//! nf-irods: the `irods` scope of a config reaches the iRODS provider that reads and writes
//! `irods://` paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::net::TcpListener;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

/// The server reads the startup message, which names the user and zone, and hangs up.
#[test]
fn scope_reaches_storage() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("an address").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut length = [0; 4];
        stream.read_exact(&mut length).expect("a header length");
        let mut header = vec![0; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut header).expect("a header");
        let header = String::from_utf8_lossy(&header).into_owned();
        let start = header.find("<msgLen>").expect("a length") + "<msgLen>".len();
        let end = header.find("</msgLen>").expect("a length");
        let mut body = vec![0; header[start..end].parse().expect("a number")];
        stream.read_exact(&mut body).expect("a body");
        String::from_utf8_lossy(&body).into_owned()
    });
    let config = loaded(&format!(
        "plugins {{ id 'nf-irods@0.1.2' }}\nirods {{\n    host = '127.0.0.1'\n    port = {port}\n    \
         username = 'rods'\n    password = 'rods'\n    zone = 'tempZone'\n}}\n"
    ));
    let observers = catalogue::observers(&config).expect("an activated plugin");
    assert!(observers.is_empty());
    let storage = storage::Storage::default().blocking();
    let error = storage
        .stat("irods:///tempZone/home/rods/a.txt")
        .expect_err("a hang-up");
    assert!(error.to_string().contains("iRODS"), "{error}");
    let startup = server.join().expect("the server");
    assert!(startup.contains("<proxyUser>rods</proxyUser>"), "{startup}");
    assert!(
        startup.contains("<proxyRcatZone>tempZone</proxyRcatZone>"),
        "{startup}"
    );
    let config = loaded("plugins { id 'nf-irods' }\nirods.sslNegotiationPolicy = 'CS_NEG_MAYBE'\n");
    catalogue::observers(&config).expect("an activated plugin");
    let storage = storage::Storage::default().blocking();
    let error = storage
        .stat("irods:///tempZone/a.txt")
        .expect_err("an unknown policy");
    assert!(error.to_string().contains("CS_NEG_MAYBE"), "{error}");
}
