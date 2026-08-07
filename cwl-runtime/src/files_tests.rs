//! Tests of file objects: derived names, secondary file patterns, literals and listings.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn splits_extensions_as_python_does() {
    assert_eq!(split_extension("a.tar.gz"), ("a.tar", ".gz"));
    assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
    assert_eq!(split_extension("a."), ("a", "."));
    assert_eq!(split_extension("noext"), ("noext", ""));
}

#[test]
fn substitutes_secondary_patterns() {
    assert_eq!(substitute("reads.bam", ".bai"), "reads.bam.bai");
    assert_eq!(substitute("reads.bam", "^.bai"), "reads.bai");
    assert_eq!(substitute("ref.fa.gz", "^^.dict"), "ref.dict");
    assert_eq!(substitute("ref", "^.fai"), "ref.fai");
}

#[test]
fn normalizes_files_and_literals() {
    let mut value = Value::parse(
        "[{class: File, location: 'file:///data/a%20b.fastq.gz'}, \
         {class: File, basename: x.txt, contents: hi}]",
    )
    .expect("YAML");
    normalize(&mut value).expect("valid files");
    let files = value.as_list().expect("a list");
    assert_eq!(files[0].text("basename"), Some("a b.fastq.gz"));
    assert_eq!(files[0].text("nameroot"), Some("a b.fastq"));
    assert_eq!(files[0].text("nameext"), Some(".gz"));
    assert!(
        files[1]
            .text("location")
            .is_some_and(|l| l.starts_with("_:"))
    );
    assert_eq!(files[1].text("nameroot"), Some("x"));
}

#[test]
fn resolves_locations_against_the_job() {
    let mut value = Value::parse("{f: {class: File, path: sub/x.txt}}").expect("YAML");
    resolve_locations(&mut value, "file:///work/job.yml");
    let file = value.get("f").expect("f");
    assert_eq!(file.text("location"), Some("file:///work/sub/x.txt"));
    assert!(file.get("path").is_none());
    // A location is a URI reference, whose escapes stay; a path is a path.
    let text = "{l: {class: File, location: A%3AGln2Cys}, p: {class: File, path: 'a%b c'}}";
    let mut value = Value::parse(text).expect("YAML");
    resolve_locations(&mut value, "file:///work/job.yml");
    let location = value.get("l").and_then(|file| file.text("location"));
    assert_eq!(location, Some("file:///work/A%3AGln2Cys"));
    let local = location.and_then(local_path).expect("a local path");
    assert_eq!(local, std::path::PathBuf::from("/work/A:Gln2Cys"));
    let path = value.get("p").and_then(|file| file.text("location"));
    assert_eq!(
        path.and_then(local_path).expect("a path").to_str(),
        Some("/work/a%b c")
    );
}

#[test]
fn absent_files_count_as_empty_in_a_simulated_run() {
    let mut file = Value::parse("{class: File, location: 'file:///nowhere/x'}").expect("YAML");
    let map = file.as_map_mut().expect("a map");
    let simulated = Access {
        assume_present: true,
        ..Access::default()
    };
    simulated.load_contents(map).expect("contents");
    simulated.add_size(map);
    assert_eq!(map.get("contents"), Some(&Value::string("")));
    assert_eq!(map.get("size"), Some(&Value::Integer(0)));
    assert!(Access::default().read(map, None).is_err());
}

/// A web server on a local port that answers only requests with the bearer token `t0k3n`. It
/// serves `/data.txt` (13 bytes) and `/big.txt` (70,000 bytes), and counts its requests.
fn serve() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let (mut allowed, mut line) = (false, String::new());
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                allowed |= line
                    .trim()
                    .eq_ignore_ascii_case("authorization: Bearer t0k3n");
                line.clear();
            }
            let body = match first.split(' ').nth(1) {
                Some("/data.txt") => "hello remote\n".to_owned(),
                Some("/big.txt") => "x".repeat(70_000),
                _ => String::new(),
            };
            let status = match (allowed, body.is_empty()) {
                (false, _) => "401 Unauthorized",
                (true, true) => "404 Not Found",
                (true, false) => "200 OK",
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if !first.starts_with("HEAD") {
                let _ = stream.write_all(body.as_bytes());
            }
        }
    });
    (address, requests)
}

#[test]
fn contents_and_sizes_of_files_at_addresses_come_through_the_storage_providers() {
    use std::sync::atomic::Ordering;
    let (server, requests) = serve();
    let text = format!(
        "[storage.data]\ntype = \"https\"\nprefixes = [\"{server}/\"]\ntoken = \"t0k3n\"\n"
    );
    let config = config_file::ConfigFile::parse(&text, Path::new("config.toml")).expect("TOML");
    let storage = storage::Storage::from_config(&config).expect("valid settings");
    let downloads = tempfile::tempdir().expect("a folder");
    let remote = Remote::new(&storage, downloads.path().to_owned());
    let real = Access {
        assume_present: false,
        remote: remote.clone(),
    };
    let file = |name: &str| {
        Value::parse(&format!("{{class: File, location: '{server}/{name}'}}")).expect("YAML")
    };

    let mut data = file("data.txt");
    let map = data.as_map_mut().expect("a map");
    real.add_size(map);
    real.load_contents(map).expect("the provider has a token");
    assert_eq!(map.get("size"), Some(&Value::Integer(13)));
    assert_eq!(map.get("contents"), Some(&Value::string("hello remote\n")));

    // A file over the limit of `loadContents` is refused before it is downloaded.
    let mut big = file("big.txt");
    let map = big.as_map_mut().expect("a map");
    let error = real.load_contents(map).expect_err("too large");
    assert!(error.contains("is larger than 65536 bytes"), "{error}");
    let error = real.load_contents(file("none.txt").as_map_mut().expect("a map"));
    assert!(error.expect_err("missing").contains("does not exist"));

    // Without the provider of the config file, the server refuses the token-less request.
    let plain = Access {
        assume_present: false,
        remote: Remote::new(&storage::Storage::default(), downloads.path().join("plain")),
    };
    assert!(
        plain
            .load_contents(file("data.txt").as_map_mut().expect("a map"))
            .is_err()
    );

    // A simulated run takes the file as present and empty, and sends no request.
    let before = requests.load(Ordering::SeqCst);
    let simulated = Access {
        assume_present: true,
        remote,
    };
    let mut other = file("other.txt");
    let map = other.as_map_mut().expect("a map");
    simulated.add_size(map);
    simulated.load_contents(map).expect("empty contents");
    assert_eq!(map.get("size"), Some(&Value::Integer(0)));
    assert_eq!(requests.load(Ordering::SeqCst), before);
}
