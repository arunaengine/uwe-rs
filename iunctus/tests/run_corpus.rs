//! Runs every nf-core pipeline of the corpus with `-profile test` and the mock executor, and
//! compares the outcome with `tests/fixtures/run_corpus.txt`: `ok`, or the exit code and the
//! first line of the error. Run with `just run-corpus-test`; on a difference the test prints
//! the lines of the new fixture.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;

use fancy_regex::Regex;

/// The outcome of one run: `ok`, or the exit code and the first error line without the
/// parts that differ between machines and runs.
fn outcome(corpus: &Path, pipeline: &str, cache: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_iunctus"))
        .args(["run", &corpus.join(pipeline).to_string_lossy()])
        .args(["-profile", "test", "-executor", "mock"])
        .args(["--outdir", &cache.join("out").to_string_lossy()])
        .env("XDG_CACHE_HOME", cache)
        .current_dir(cache)
        .output()
        .expect("runs iunctus");
    let code = output.status.code().unwrap_or(-1);
    if code == 0 {
        return "ok".into();
    }
    let error = String::from_utf8_lossy(&output.stderr);
    let first = error.lines().next().unwrap_or_default();
    let prefix = format!("{}/", corpus.join(pipeline).display());
    let first = first
        .replace(&prefix, "")
        .replace(&cache.display().to_string(), "<cache>");
    let run = Regex::new(r"[0-9a-f]{9,}-[0-9a-f]+").expect("a pattern");
    let time = Regex::new(r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}").expect("a pattern");
    let address = Regex::new(r"@[0-9a-f]{6,}").expect("a pattern");
    let first = run.replace_all(&first, "<run>");
    let first = address.replace_all(&first, "@<address>");
    let first = time.replace_all(&first, "<time>");
    format!("{code} {first}")
}

#[test]
#[ignore = "needs the nf-core corpus and the network; run `just run-corpus-test`"]
fn pipelines_run_as_recorded() {
    let corpus = PathBuf::from(std::env::var("NF_CORE_CORPUS").expect("NF_CORE_CORPUS is set"))
        .join("pipelines");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_corpus.txt");
    let expected = std::fs::read_to_string(&fixture).expect("reads the fixture");
    let expected: Vec<&str> = expected
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let mut pipelines: Vec<String> = std::fs::read_dir(&corpus)
        .expect("reads the corpus")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    pipelines.sort();
    // Downloads are shared with the user's cache, so that runs do not fetch them again.
    let cache = tempfile::tempdir().expect("a temporary directory");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is set");
    let downloads = home.join(".cache/iunctus/downloads");
    std::fs::create_dir_all(&downloads).expect("creates the downloads directory");
    std::fs::create_dir_all(cache.path().join("iunctus")).expect("creates the cache");
    std::os::unix::fs::symlink(&downloads, cache.path().join("iunctus/downloads"))
        .expect("links the downloads");
    let actual: Vec<String> = pipelines
        .iter()
        .map(|pipeline| format!("{pipeline} {}", outcome(&corpus, pipeline, cache.path())))
        .collect();
    if actual != expected {
        let changed: Vec<String> = actual
            .iter()
            .filter(|line| !expected.contains(&line.as_str()))
            .cloned()
            .collect();
        panic!(
            "{} pipelines differ:\n{}\n\nThe new fixture:\n{}",
            changed.len(),
            changed.join("\n"),
            actual.join("\n")
        );
    }
}
