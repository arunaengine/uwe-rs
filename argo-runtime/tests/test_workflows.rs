//! Reads every Argo document of the test workflows from `just argo-test-workflows`. Documents
//! with an entrypoint run with the mock TES executor; the others, like libraries of
//! WorkflowTemplates, get the checks Argo makes when they are created. The outcomes are compared
//! with tests/fixtures/test-workflows/outcomes.txt: `ok` and the number of tasks, `valid`, or the
//! exit code (33 for unsupported features) and the first error. Each document that does not pass
//! needs a line in tests/fixtures/test-workflows/rejected.txt with the evidence that Argo rejects
//! or fails it too. `ARGO_TEST_WORKFLOWS_UPDATE=1` rewrites the outcomes;
//! `ARGO_TEST_WORKFLOWS_FILTER` runs only the documents whose path contains it, and
//! `ARGO_TEST_WORKFLOWS_ORDER=shuffled:<seed>` lets tasks finish in a random order.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_runtime::library::collect_files;
use argo_runtime::{ArgoDriver, Document, Kind, Library, Options, load_file, validate};
use workflow_graph::{MockTes, Order, RunOptions, execute_with};

const FIXTURE: &str = "tests/fixtures/test-workflows/outcomes.txt";

/// Why the documents that do not pass fail in Argo too, by document.
const REJECTED: &str = "tests/fixtures/test-workflows/rejected.txt";

/// Directories of templates that documents under a path get as configs, as `Launch.configs`
/// gives them. Hera's upstream examples are copies of Argo's examples, but Hera does not copy
/// the WorkflowTemplates that they refer to in full. The talk-demos examples of plugin templates
/// install the hello executor plugin from its own repository, and the talk-demos copy of the CI
/// workflow of pipekit/argo-workflows-ci-example refers to that repository's WorkflowTemplates.
const CONFIGS: &[(&str, &str)] = &[
    (
        "hera/examples/workflows/upstream/",
        "argo-workflows/examples",
    ),
    (
        "talk-demos/argocon-demos/2025-argo-workflow-templates-a-practical-deep-dive/",
        "argo-workflows-hello-executor-plugin",
    ),
    (
        "talk-demos/argocon-demos/2023-artifact-management-at-scale/",
        "argo-workflows-ci-example/bootstrap/workflow-templates",
    ),
];

#[test]
#[ignore = "needs the Argo test workflows from `just argo-test-workflows`; run it with `just argo-test-workflows-test`"]
fn runs_the_test_workflows() {
    // Recursive workflows nest templates 100 levels deep before the depth limit stops them,
    // which needs more stack than a test thread has.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run_test_workflows)
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

/// The test workflows, the options of the test, and what the runs share.
struct TestWorkflows {
    root: PathBuf,
    filter: String,
    /// How tasks finish: in the order asked for, or up to 8 at a time in another order.
    options: RunOptions,
    verbose: bool,
    scratch: tempfile::TempDir,
    /// The documents of the directories in [`CONFIGS`], read once.
    configs: BTreeMap<&'static str, Vec<Rc<Document>>>,
}

impl TestWorkflows {
    /// The library of a document: its own file first, then the configs for its path, then the
    /// documents of its repository.
    fn library(&mut self, relative: &str, own: &[Rc<Document>], pool: &[Rc<Document>]) -> Library {
        let library = Library::new();
        library.add_documents(0, own.iter().cloned());
        for (prefix, directory) in CONFIGS {
            if relative.starts_with(prefix) {
                let root = &self.root;
                let config = self.configs.entry(directory).or_insert_with(|| {
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
        library
    }

    /// Runs a document with the mock TES executor: `ok` and the number of tasks, or the exit
    /// code and the error.
    fn run(&self, relative: &str, document: &Rc<Document>, library: Library) -> String {
        let options = Options {
            scratch: self.scratch.path().join("scratch"),
            now: Some("2026-09-28T12:00:00Z".parse().expect("a time")),
            assume_inputs: true,
            ..Options::default()
        };
        let mut driver = ArgoDriver::new(Rc::clone(document), Rc::new(library), options);
        let work = self.scratch.path().join("mock");
        let _ = std::fs::remove_dir_all(&work);
        let mut executor = MockTes::new(&work);
        let execution = execute_with(relative, &mut driver, &mut executor, &self.options);
        let outcome = match &execution.error {
            None => {
                if let Err(message) = execution.graph.check() {
                    panic!("{relative}: the graph is not valid: {message}");
                }
                format!("ok {}", execution.graph.nodes.len())
            }
            Some(error) => {
                let code = if error.unsupported() { 33 } else { 1 };
                format!("{code} {}", one_line(&error.to_string(), ""))
            }
        };
        if self.verbose {
            println!("{relative} {outcome}");
            for warning in driver.warnings() {
                println!("    warning: {warning}");
            }
        }
        outcome
    }
}

/// A message on one line without the folder of the test workflows. A message of more than 200 characters
/// keeps its end, where Argo's messages name what failed.
fn one_line(message: &str, root: &str) -> String {
    let mut message = message.replace(['\n', '\r'], " ");
    if !root.is_empty() {
        message = message.replace(root, "");
    }
    let characters: Vec<char> = message.chars().collect();
    if characters.len() <= 200 {
        return message;
    }
    let end: String = characters[characters.len() - 197..].iter().collect();
    format!("...{end}")
}

/// The run options that `ARGO_TEST_WORKFLOWS_ORDER` asks for: `reversed` or `shuffled:<seed>`
/// let up to 8 tasks be pending and finish in that order, which shows whether the driver
/// depends on the order of outcomes; anything else runs one task at a time.
fn run_options(order: &str) -> RunOptions {
    let order = match order.split_once(':') {
        Some(("shuffled", seed)) => Order::Shuffled {
            seed: seed.parse().expect("a number as the seed"),
        },
        _ if order == "reversed" => Order::Reversed,
        _ => return RunOptions::default(),
    };
    RunOptions {
        max_pending: 8,
        order,
    }
}

/// The folders of the test workflows, one per repository.
fn repositories(root: &Path) -> Vec<PathBuf> {
    let mut repositories: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("the folder of the test workflows is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    repositories.sort();
    repositories
}

/// The outcomes of a fixture file, by document.
fn read_fixture(fixture: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(fixture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .map(|(document, outcome)| (document.to_owned(), outcome.to_owned()))
        .collect()
}

fn run_test_workflows() {
    let root =
        PathBuf::from(std::env::var("ARGO_TEST_WORKFLOWS").expect("ARGO_TEST_WORKFLOWS is set"));
    let update = std::env::var("ARGO_TEST_WORKFLOWS_UPDATE").is_ok_and(|value| value == "1");
    let trace = std::env::var("ARGO_TEST_WORKFLOWS_TRACE").is_ok();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let expected = read_fixture(&fixture);
    let mut workflows = TestWorkflows {
        root: root.clone(),
        filter: std::env::var("ARGO_TEST_WORKFLOWS_FILTER").unwrap_or_default(),
        options: run_options(&std::env::var("ARGO_TEST_WORKFLOWS_ORDER").unwrap_or_default()),
        verbose: std::env::var("ARGO_TEST_WORKFLOWS_VERBOSE").is_ok(),
        scratch: tempfile::tempdir().expect("a temporary directory"),
        configs: BTreeMap::new(),
    };
    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();
    let mut summary: Vec<String> = Vec::new();
    for repository in repositories(&root) {
        let mut files = Vec::new();
        collect_files(&repository, &mut files);
        let mut pool = Vec::new();
        for file in &files {
            if trace {
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
            if !relative.contains(&workflows.filter) {
                continue;
            }
            let documents = match load_file(&file) {
                Ok(documents) => documents,
                Err(error) => {
                    // A file that does not load counts when it looks like it holds Argo documents.
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    if text.contains("apiVersion: argoproj.io/") {
                        total += 1;
                        let message = one_line(&error, &root.to_string_lossy());
                        if workflows.verbose {
                            println!("{relative} 1 {message}");
                        }
                        outcomes.insert(relative.clone(), format!("1 {message}"));
                    }
                    continue;
                }
            };
            // ConfigMaps are no workflows; they serve the documents that read them.
            let checked: Vec<&Rc<Document>> = documents
                .iter()
                .filter(|document| document.kind != Kind::ConfigMap)
                .collect();
            for document in checked.iter().copied() {
                let key = if checked.len() > 1 {
                    format!("{relative}#{}", document.index)
                } else {
                    relative.clone()
                };
                if trace {
                    eprintln!("running {key}");
                }
                let library = workflows.library(&relative, &documents, &pool);
                let outcome = if document.runnable() {
                    workflows.run(&relative, document, library)
                } else {
                    match validate(document, &library) {
                        Ok(()) => "valid".to_owned(),
                        Err(rejection) => {
                            let code = if rejection.unsupported { 33 } else { 1 };
                            format!("{code} {}", one_line(&rejection.message, ""))
                        }
                    }
                };
                total += 1;
                if passed(&outcome) {
                    ok += 1;
                }
                outcomes.insert(key, outcome);
            }
        }
        let name = repository.file_name().unwrap_or_default().to_string_lossy();
        if total > 0 {
            summary.push(format!("{name:40} {ok:5} of {total:5} run or pass"));
        }
    }
    let total = outcomes.len();
    let ok = outcomes.values().filter(|outcome| passed(outcome)).count();
    println!("{}", summary.join("\n"));
    println!("{ok} of {total} documents run through with a full graph or pass Argo's checks");
    if update {
        write_fixture(&fixture, outcomes, &expected, &workflows.filter);
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
    if workflows.filter.is_empty() {
        for document in expected.keys() {
            if !outcomes.contains_key(document) {
                differences.push(format!(
                    "{document}: recorded, but not in the test workflows"
                ));
            }
        }
        assert!(
            total > 1_000,
            "incomplete test workflows: {total} documents"
        );
    }
    assert!(
        differences.is_empty(),
        "{} documents differ from {FIXTURE}; ARGO_TEST_WORKFLOWS_UPDATE=1 records them:\n{}",
        differences.len(),
        differences.join("\n")
    );
    if workflows.filter.is_empty() {
        check_evidence(&outcomes);
    }
}

/// Whether a document runs through or passes Argo's checks.
fn passed(outcome: &str) -> bool {
    outcome.starts_with("ok") || outcome == "valid"
}

/// Checks that every document that does not pass has evidence that Argo rejects or fails it
/// too, and that no evidence is left for a document that passes.
fn check_evidence(outcomes: &BTreeMap<String, String>) {
    let evidence = read_fixture(&Path::new(env!("CARGO_MANIFEST_DIR")).join(REJECTED));
    let mut problems = Vec::new();
    for (document, outcome) in outcomes {
        if !passed(outcome) && !evidence.contains_key(document) {
            problems.push(format!("{document}: {outcome} (no evidence)"));
        }
    }
    for document in evidence.keys() {
        if outcomes.get(document).is_none_or(|outcome| passed(outcome)) {
            problems.push(format!("{document}: evidence, but the document passes"));
        }
    }
    assert!(
        problems.is_empty(),
        "{} documents disagree with {REJECTED}:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// Writes the outcomes; a filtered run updates only the documents it ran.
fn write_fixture(
    fixture: &Path,
    mut outcomes: BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
    filter: &str,
) {
    if !filter.is_empty() {
        for (document, outcome) in expected {
            if !document.contains(filter) {
                outcomes.insert(document.clone(), outcome.clone());
            }
        }
    }
    let mut text = String::from(
        "# Outcomes of the Argo test-workflows test: `ok` and the number of tasks of a run with the mock TES\n# executor, `valid` for a document without an entrypoint that passes Argo's checks, or the\n# exit code (33 for unsupported features) and the first error. A tab ends the path, which may\n# hold spaces.\n",
    );
    for (document, outcome) in &outcomes {
        text.push_str(&format!("{document}\t{outcome}\n"));
    }
    std::fs::create_dir_all(fixture.parent().expect("a directory")).expect("the fixture directory");
    std::fs::write(fixture, text).expect("the fixture is written");
}
