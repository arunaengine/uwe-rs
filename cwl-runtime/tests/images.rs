//! Checks CWL image selection, content-addressed recipes, saved archives and remote refusal.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch};

fn image(root: &std::path::Path, fields: &str, options: Options) -> Result<String, String> {
    image_in(root, "requirements", fields, options)
}

/// The image of a tool whose DockerRequirement is in `section`, `requirements` or `hints`; an
/// error of an unsupported feature starts with `unsupported: `.
fn image_in(
    root: &std::path::Path,
    section: &str,
    fields: &str,
    options: Options,
) -> Result<String, String> {
    std::fs::write(
        root.join("main.cwl"),
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\n\
        baseCommand: echo\n{section}:\n  DockerRequirement:\n{fields}\ninputs: []\noutputs: []\n"
        ),
    )
    .unwrap();
    let mut driver = CwlDriver::new(
        &Launch {
            target: root.join("main.cwl"),
            ..Launch::default()
        },
        options,
    )
    .map_err(|error| error.message)?;
    let mut tasks = driver.start().map_err(|error| match error.unsupported {
        true => format!("unsupported: {}", error.message),
        false => error.message,
    })?;
    Ok(tasks.remove(0).image.unwrap())
}

#[test]
fn hashes_build_context() {
    let root = tempfile::tempdir().unwrap();
    let fields = "    dockerFile: 'FROM scratch'\n    dockerImageId: declared:tag\n    dockerContext: context";
    std::fs::create_dir(root.path().join("context")).unwrap();
    std::fs::write(root.path().join("context/seed"), "one").unwrap();
    let first = image(root.path(), fields, Options::default()).unwrap();
    assert!(first.starts_with("iunctus-cwl:build-"));
    assert_eq!(
        image(root.path(), fields, Options::default()).unwrap(),
        first
    );
    std::fs::write(root.path().join("context/seed"), "two").unwrap();
    assert_ne!(
        image(root.path(), fields, Options::default()).unwrap(),
        first
    );
}

#[test]
fn hashes_image_archives() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("image.tar");
    for field in ["dockerLoad", "dockerImport"] {
        std::fs::write(&archive, "one").unwrap();
        let fields = format!("    {field}: image.tar\n    dockerImageId: declared:tag");
        let first = image(root.path(), &fields, Options::default()).unwrap();
        assert!(first.starts_with("iunctus-cwl:"));
        assert_eq!(
            image(root.path(), &fields, Options::default()).unwrap(),
            first
        );
        std::fs::write(&archive, "two").unwrap();
        assert_ne!(
            image(root.path(), &fields, Options::default()).unwrap(),
            first
        );
    }
}

#[test]
fn refuses_remote_preparation() {
    let root = tempfile::tempdir().unwrap();
    let fields = [
        "    dockerFile: 'FROM scratch'",
        "    dockerLoad: missing.tar",
        "    dockerImport: missing.tar",
    ];
    // As in cwltool, a hint fails too, rather than run in the default image, but only a
    // requirement is unsupported.
    for (section, fields) in ["requirements", "hints"]
        .into_iter()
        .flat_map(|section| fields.map(|fields| (section, fields)))
    {
        let remote = Options {
            simulated: false,
            ..Options::default()
        };
        let error = image_in(root.path(), section, fields, remote).unwrap_err();
        assert!(error.contains("`[cwl]` section"), "{error}");
        assert!(error.contains("SLURM with Apptainer"), "{error}");
        let unsupported = error.starts_with("unsupported: ");
        assert_eq!(unsupported, section == "requirements", "{error}");
    }
}

/// The image ID that the fake engine loads.
const LOADED: &str = "sha256:abababababababababababababababababababababababababababababababab";

/// A container engine at a socket in `folder` that loads one image, [`LOADED`], and saves it as
/// `saved image`. It notes the path of each request.
fn fake_engine(folder: &std::path::Path) -> (String, Arc<Mutex<Vec<String>>>) {
    let socket = folder.join("engine.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        let mut loaded = false;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let path = request(&mut reader);
            seen.lock().unwrap().push(path.clone());
            let (status, body) = if path.contains("/images/load") {
                loaded = true;
                let line = format!("{{\"stream\":\"Loaded image ID: {LOADED}\\n\"}}\n");
                ("200 OK", line)
            } else if path.ends_with("/get") {
                ("200 OK", "saved image".to_owned())
            } else if path.contains("/tag?") {
                ("201 Created", String::new())
            } else if path.ends_with("/json") && path.contains("/images/") && !loaded {
                ("404 Not Found", "{\"message\":\"missing\"}".to_owned())
            } else if path.ends_with("/json") && path.contains("/images/") {
                ("200 OK", format!("{{\"Id\":\"{LOADED}\"}}"))
            } else if path.ends_with("/version") {
                (
                    "200 OK",
                    "{\"Version\":\"27.0.0\",\"ApiVersion\":\"1.47\"}".to_owned(),
                )
            } else {
                ("200 OK", "{}".to_owned())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(format!("{head}{body}").as_bytes());
        }
    });
    (socket.to_string_lossy().into_owned(), requests)
}

/// Reads one request, with its body, and gives its path.
fn request(reader: &mut BufReader<std::os::unix::net::UnixStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let (mut length, mut chunked) = (0, false);
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap_or_default();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap(),
            "transfer-encoding" => chunked = value.trim() == "chunked",
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    while chunked {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let size = usize::from_str_radix(line.trim(), 16).unwrap();
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).unwrap();
        chunked = size > 0;
    }
    path
}

#[test]
fn saves_slurm_archives() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("image.tar"), "load me").unwrap();
    let (socket, requests) = fake_engine(root.path());
    let mut javascript = cwl_runtime::JavaScriptSettings::default();
    javascript.engine = socket;
    let archives = root.path().join("archives");
    let options = || Options {
        simulated: false,
        javascript: javascript.clone(),
        archives: Some(archives.clone()),
        ..Options::default()
    };
    let fields = "    dockerLoad: image.tar";
    let held = options();
    let first = image(root.path(), fields, held.clone()).unwrap();
    // The run holds the folder, so that clean-up removes nothing that it uses.
    assert!(storage::hold_downloads(&archives).unwrap().is_none());
    drop(held);
    // The task names the image ID, which is the same on every machine.
    let file = archives.join(LOADED.replace(':', "-") + ".tar");
    assert_eq!(first, LOADED);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved image");
    // The next run finds the image and its archive, and saves nothing again.
    assert_eq!(image(root.path(), fields, options()).unwrap(), first);
    let requests = requests.lock().unwrap();
    let saved = requests.iter().filter(|path| path.ends_with("/get"));
    assert_eq!(saved.count(), 1, "{requests:?}");
}

#[test]
#[ignore = "needs Docker and a registry at IUNCTUS_TEST_REGISTRY, like localhost:5000/cwl"]
fn pushes_remote_images() {
    let registry = std::env::var("IUNCTUS_TEST_REGISTRY").expect("IUNCTUS_TEST_REGISTRY");
    let root = tempfile::tempdir().unwrap();
    let remote = Options {
        simulated: false,
        registry: Some(registry.clone()),
        ..Options::default()
    };
    let fields = "    dockerFile: \"FROM scratch\\nLABEL org.iunctus.test=push\"";
    let pushed = image_in(root.path(), "hints", fields, remote).unwrap();
    assert!(
        pushed.starts_with(&format!("{registry}@sha256:")),
        "{pushed}"
    );
}
