//! Runs every Argo document of the corpus from `just argo-corpus` that has an entrypoint with
//! the mock TES executor, and compares the outcomes with tests/fixtures/corpus/outcomes.txt:
//! `ok` and the number of tasks, or the exit code (33 for unsupported features) and the first
//! error. `ARGO_CORPUS_UPDATE=1` rewrites the fixture; `ARGO_CORPUS_FILTER` runs only the
//! documents whose path contains it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_runtime::library::collect_files;
use argo_runtime::{ArgoDriver, Library, Options, load_file};
use workflow_graph::{MockTes, execute};

const FIXTURE: &str = "tests/fixtures/corpus/outcomes.txt";

/// Directories of templates that documents under a path get as configs, as `Launch.configs`
/// gives them. Hera's upstream examples are copies of Argo's examples, but Hera does not copy
/// the WorkflowTemplates that they refer to in full.
const CONFIGS: &[(&str, &str)] = &[(
    "hera/examples/workflows/upstream/",
    "argo-workflows/examples",
)];

#[test]
#[ignore = "needs the Argo corpus from `just argo-corpus`; run it with `just argo-corpus-test`"]
fn runs_the_corpus() {
    // Recursive workflows nest templates 100 levels deep before the depth limit stops them,
    // which needs more stack than a test thread has.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run_corpus)
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn run_corpus() {
    let root = PathBuf::from(std::env::var("ARGO_CORPUS").expect("ARGO_CORPUS is set"));
    let filter = std::env::var("ARGO_CORPUS_FILTER").unwrap_or_default();
    let update = std::env::var("ARGO_CORPUS_UPDATE").is_ok_and(|value| value == "1");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let expected: BTreeMap<String, String> = std::fs::read_to_string(&fixture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .map(|(document, outcome)| (document.to_owned(), outcome.to_owned()))
        .collect();
    let mut repositories: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the corpus directory is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    repositories.sort();
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();
    let mut summary: Vec<String> = Vec::new();
    let mut configs: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for repository in &repositories {
        let mut files = Vec::new();
        collect_files(repository, &mut files);
        let mut pool = Vec::new();
        for file in &files {
            if std::env::var("ARGO_CORPUS_TRACE").is_ok() {
                eprintln!("reading {}", file.display());
            }
            pool.extend(load_file(file).unwrap_or_default());
        }
        let (mut total, mut ok) = (0, 0);
        for file in files {
            let relative = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .to_string_lossy()
                .into_owned();
            if !relative.contains(&filter) {
                continue;
            }
            let documents = match load_file(&file) {
                Ok(documents) => documents,
                Err(error) => {
                    // A file that does not load counts when it looks like an Argo workflow.
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    if text.contains("argoproj.io/") && text.contains("entrypoint") {
                        total += 1;
                        let message: String =
                            error.replace(['\n', '\r'], " ").chars().take(200).collect();
                        let message = message.replace(&root.to_string_lossy().into_owned(), "");
                        if std::env::var("ARGO_CORPUS_VERBOSE").is_ok() {
                            println!("{relative} 1 {message}");
                        }
                        outcomes.insert(relative.clone(), format!("1 {message}"));
                    }
                    continue;
                }
            };
            let runnable: Vec<_> = documents
                .iter()
                .filter(|document| document.runnable())
                .cloned()
                .collect();
            for document in &runnable {
                let key = if documents.len() > 1 {
                    format!("{relative}#{}", document.index)
                } else {
                    relative.clone()
                };
                if std::env::var("ARGO_CORPUS_TRACE").is_ok() {
                    eprintln!("running {key}");
                }
                let library = Library::new();
                library.add_documents(0, documents.iter().cloned());
                for (prefix, directory) in CONFIGS {
                    if relative.starts_with(prefix) {
                        let config = configs.entry(directory).or_insert_with(|| {
                            let mut files = Vec::new();
                            collect_files(&root.join(directory), &mut files);
                            files
                                .iter()
                                .flat_map(|file| load_file(file).unwrap_or_default())
                                .collect()
                        });
                        library.add_documents(1, config.iter().cloned());
                    }
                }
                library.add_documents(2, pool.iter().cloned());
                let options = Options {
                    scratch: scratch.path().join("scratch"),
                    now: Some("2026-09-28T12:00:00Z".parse().expect("a time")),
                    ..Options::default()
                };
                let mut driver = ArgoDriver::new(Rc::clone(document), Rc::new(library), options);
                let work = scratch.path().join("mock");
                let _ = std::fs::remove_dir_all(&work);
                let mut executor = MockTes::new(&work);
                let execution = execute(&relative, &mut driver, &mut executor);
                total += 1;
                let outcome = match &execution.error {
                    None => {
                        ok += 1;
                        if let Err(message) = execution.graph.check() {
                            panic!("{key}: the graph is not valid: {message}");
                        }
                        format!("ok {}", execution.graph.nodes.len())
                    }
                    Some(error) => {
                        let code = if error.unsupported() { 33 } else { 1 };
                        let mut message: String = error.to_string().replace(['\n', '\r'], " ");
                        if message.chars().count() > 200 {
                            message = message.chars().take(200).collect();
                        }
                        format!("{code} {message}")
                    }
                };
                if std::env::var("ARGO_CORPUS_VERBOSE").is_ok() {
                    println!("{key} {outcome}");
                    for warning in driver.warnings() {
                        println!("    warning: {warning}");
                    }
                }
                outcomes.insert(key, outcome);
            }
        }
        let name = repository.file_name().unwrap_or_default().to_string_lossy();
        if total > 0 {
            summary.push(format!("{name:40} {ok:5} of {total:5} with a full graph"));
        }
    }
    let total = outcomes.len();
    let ok = outcomes
        .values()
        .filter(|outcome| outcome.starts_with("ok"))
        .count();
    println!("{}", summary.join("\n"));
    println!("{ok} of {total} documents run through with a full graph");
    if update {
        // A filtered run updates only the documents it ran.
        let mut outcomes = outcomes;
        if !filter.is_empty() {
            for (document, outcome) in &expected {
                if !document.contains(&filter) {
                    outcomes.insert(document.clone(), outcome.clone());
                }
            }
        }
        let mut text = String::from(
            "# Outcomes of the Argo corpus test with the mock TES executor: `ok` and the number of\n# tasks, or the exit code (33 for unsupported features) and the first error. A tab ends the\n# path, which may hold spaces.\n",
        );
        for (document, outcome) in &outcomes {
            text.push_str(&format!("{document}\t{outcome}\n"));
        }
        std::fs::create_dir_all(fixture.parent().expect("a directory"))
            .expect("the fixture directory");
        std::fs::write(&fixture, text).expect("the fixture is written");
        return;
    }
    let mut differences = Vec::new();
    for (document, outcome) in &outcomes {
        match expected.get(document) {
            Some(recorded) if recorded == outcome => {}
            Some(recorded) => {
                differences.push(format!("{document}: {outcome} (recorded: {recorded})"))
            }
            None => differences.push(format!("{document}: {outcome} (not recorded)")),
        }
    }
    if filter.is_empty() {
        for document in expected.keys() {
            if !outcomes.contains_key(document) {
                differences.push(format!("{document}: recorded, but not in the corpus"));
            }
        }
        assert!(total > 1_000, "incomplete corpus: {total} documents");
    }
    assert!(
        differences.is_empty(),
        "{} documents differ from {FIXTURE}; ARGO_CORPUS_UPDATE=1 records them:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
