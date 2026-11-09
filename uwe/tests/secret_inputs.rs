//! Compiled runs with a secret input: the value reaches the task, and no wir document, value
//! file, recorded source or crate holds it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use uwe::RunRequest;
use uwe::run::{Event, SecretValues, Settings};

/// The secret value of every run. Asserts name the files that hold it, never the value.
const SECRET: &str = "hush-4711-value";

const CWL: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [echo]\ninputs:\n  token:\n    type: string\n    inputBinding: {position: 1}\noutputs:\n  out: stdout\nstdout: out.txt\n";

const WDL: &str = "version 1.1\n\ntask hello {\n  input {\n    String token\n  }\n  command <<<\n    echo ~{token}\n  >>>\n  output {\n    File out = stdout()\n  }\n}\n";

const SNAKEFILE: &str =
    "rule all:\n    output: \"a.txt\"\n    shell: \"test -n {config[token]} && touch {output}\"\n";

/// A pipeline whose compiled document would hold the value in a channel and a default.
const NEXTFLOW: &str = "params.token = 'none'\nprocess SAY {\n    input:\n    val word\n    output:\n    path 'out.txt'\n    script:\n    \"test -n ${word} && touch out.txt\"\n}\nworkflow {\n    SAY(Channel.of(params.token))\n}\n";

#[test]
fn hides_secret_inputs() {
    let root = tempfile::tempdir().unwrap();
    for (file, text) in [
        ("tool.cwl", CWL),
        ("hello.wdl", WDL),
        ("Snakefile", SNAKEFILE),
        ("main.nf", NEXTFLOW),
    ] {
        let folder = root.path().join(file.replace('.', "-"));
        run(&folder, file, text);
    }
}

/// Runs `text` as `file` in `folder` with the secret as the input `token`, and checks where
/// the value is while the run lives and after it ended.
fn run(folder: &Path, file: &str, text: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join(file), text).unwrap();
    let compiled = folder.join("cache/scratch/lowered");
    let mut settings = Settings::new(folder.join(file));
    settings.executor = Some("simulator".into());
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache_folder = Some(folder.join("cache"));
    settings.inputs = Some(json!({"token": SECRET}));
    let mut secrets = SecretValues::default();
    secrets.insert("HOST_INPUT_SECRET", SECRET).unwrap();
    secrets.allow_environment(false);
    settings.secrets = secrets;
    settings.lowered = true;
    // The value files of a run live as long as it does.
    let (seen, held) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (listened, found) = (seen.clone(), held.clone());
    let place = compiled.clone();
    settings.listener = Some(Arc::new(move |event| {
        if let Event::Task(_) = event {
            listened.store(true, Ordering::SeqCst);
            found.lock().unwrap().extend(holding(&place));
        }
    }));
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let ran = request.run_blocking();
    let ran = ran.unwrap_or_else(|failure| panic!("{file}: {}", failure.message));
    assert!(
        ran.execution.error.is_none(),
        "{file}: {:?}",
        ran.execution.error
    );
    assert!(seen.load(Ordering::SeqCst), "{file}: no task event");
    assert_eq!(
        *held.lock().unwrap(),
        Vec::<String>::new(),
        "{file} while it ran"
    );
    let database = run_database::Database::open(&folder.join("runs.sqlite")).unwrap();
    let record = database.run(&ran.id).unwrap().unwrap();
    assert!(
        record.workflow.ends_with(".wir.json"),
        "{file}: {}",
        record.workflow
    );
    // The value reaches the task as in a run without wir.
    let tasks = database.tasks(&ran.id).unwrap();
    let described = (tasks.iter())
        .map(|row| database.task(&ran.id, row.id).unwrap().unwrap())
        .map(|detail| serde_json::to_string(&detail.description).unwrap());
    assert!(
        described.into_iter().any(|text| text.contains(SECRET)),
        "{file}: no task got the value"
    );
    // The recorded sources are the document alone, not the file of the inputs.
    let sources = database.sources(&ran.id).unwrap();
    let paths: Vec<&str> = sources.iter().map(|source| source.path.as_str()).collect();
    assert_eq!(paths.len(), 1, "{file}: {paths:?}");
    let options = run_crate::Options {
        workflow: record.workflow.clone().into(),
        secrets: vec![SECRET.to_owned()],
        ..run_crate::Options::default()
    };
    run_crate::export(&database, &ran.id, &folder.join("crate"), &options).unwrap();
    // The files of tasks in `data` hold what the tasks got, as in a run without wir.
    let tasks = folder.join("crate/data").display().to_string();
    let mut found = holding(&folder.join("crate"));
    found.retain(|path| !path.starts_with(&tasks));
    found.extend(holding(&compiled));
    found.extend(holding(&folder.join("store")));
    assert_eq!(found, Vec::<String>::new(), "{file} after the run");
}

/// The files below `folder` that hold the secret value.
fn holding(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap_or_default();
            if bytes
                .windows(SECRET.len())
                .any(|part| part == SECRET.as_bytes())
            {
                found.push(path.display().to_string());
            }
        }
    }
    found
}
