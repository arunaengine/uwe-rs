//! nf-blocks: `fromStore` over the store that nf-blocks 0.11.0-beta.1 wrote, and the blocks
//! and Store Log entry of runs, which a later `fromStore` reads back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::{Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue;
use nextflow_plugins::observer::{End, Event, Output, Start};
use storage::cbor::{Cid, Node};

/// The store `lab` of the storage crate's fixtures, which the tests only read.
fn recorded() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../storage/tests/fixtures/cas_store")
}

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn config(mine: &Path) -> String {
    format!(
        "plugins {{ id 'nf-blocks@0.11.0-beta.1' }}\nlineage.enabled = true\n\
         lineage.store.location = 'cas://mine'\noutputDir = 'cas://mine'\ncas {{\n    stores {{\n        \
         mine {{ location = '{}' }}\n        lab {{ location = '{}' }}\n    }}\n    \
         resolve = ['mine', 'lab']\n    asserted_by = 'tester@example.org'\n    pipeline = 'consumer'\n}}\n",
        mine.display(),
        recorded().display()
    )
}

/// What a run of `script` printed; uwe prints a remote path without its scheme, where the
/// plugin printed `cas://<cid>/<name>`.
fn run(folder: &Path, script: &str) -> String {
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "probe");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    printed
}

/// The newest RunCompletion of the store `mine`.
fn newest(mine: &Path) -> Node {
    let mut entries: Vec<String> = std::fs::read_dir(mine.join("log"))
        .expect("a Store Log")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    let cid = entries[0]
        .split_once("-run-")
        .expect("a run entry")
        .1
        .to_owned();
    let block = std::fs::read(mine.join(format!("blocks/{}/{cid}", &cid[cid.len() - 2..])));
    storage::cbor::decode(&block.expect("the block")).expect("DAG-CBOR")
}

fn start() -> Start {
    Start {
        run_name: "producer".into(),
        session_id: "4aa74591-7adc-42aa-8e5d-6874f2de1738".into(),
        launch_dir: "/launch".into(),
        work_dir: "/launch/work".into(),
        project_dir: "/launch".into(),
        script: "/launch/main.nf".into(),
        project_name: "launch".into(),
        repository: None,
        commit_id: None,
        revision: None,
        command_line: "uwe run main.nf".into(),
        profile: "standard".into(),
        config_files: Vec::new(),
        params: serde_json::json!({ "samples": 2 }),
        config: serde_json::json!({}),
        manifest: serde_json::json!({}),
        processes: Vec::new(),
        start: 1_793_964_945_628,
        resume: false,
    }
}

#[test]
fn records_store_runs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let mine = folder.join("mine");
    std::fs::write(folder.join("nextflow.config"), config(&mine)).expect("writes the config");
    let consumer = "include { fromStore } from 'plugin/nf-blocks'\n\nworkflow {\n    \
                    channel.fromStore(run: 'latest', pipeline: 'probe-pipeline', output: 'aligned', \
                    where: [sample: 'B']).view { \"item: ${it}\" }\n}\n";
    let printed = run(folder, consumer);
    let expected =
        "item: [[sample:B], /bafkreifsfjlickyg7pec4omfkjcxteycwag6af4clpsuh2ur2jrmja567m/B.bam]";
    assert!(printed.contains(expected), "{printed}");
    let completion = newest(&mine);
    assert_eq!(
        completion.get("status").and_then(Node::text),
        Some("succeeded")
    );
    assert_eq!(completion.get("collections"), Some(&Node::List(Vec::new())));

    // A run that published `A/A.bam` as its output `aligned`.
    let file = folder.join("A.bam");
    std::fs::write(&file, "bam-A\n").expect("a file");
    let storage = storage::Storage::default().blocking();
    storage
        .upload(&file, "cas://mine/A/A.bam")
        .expect("a publish");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: folder.to_string_lossy().into_owned(),
        launch_dir: folder.to_string_lossy().into_owned(),
        work_dir: folder.join("work").to_string_lossy().into_owned(),
    };
    let loaded: Config =
        nextflow_config::load(&[folder.join("nextflow.config")], &options, &mut Quiet)
            .expect("loads");
    let mut observers = catalogue::observers(&loaded).expect("an observer");
    let end = End {
        success: true,
        error: None,
        counts: Default::default(),
        task: None,
        outputs: vec![Output {
            name: "aligned".into(),
            files: vec!["cas://mine/A/A.bam".into()],
            index: None,
        }],
        end: 1_793_964_946_647,
    };
    for observer in &mut observers {
        observer.notify(&Event::RunStarted(Box::new(start())));
        observer.notify(&Event::RunEnded(end.clone()));
    }
    let completion = newest(&mine);
    let Some(Node::Link(manifest)) = completion.get("run") else {
        panic!("a run link");
    };
    let manifest =
        std::fs::read(mine.join(format!("blocks/{}/{manifest}", &manifest.to_string()[57..])));
    let manifest = storage::cbor::decode(&manifest.expect("the manifest")).expect("DAG-CBOR");
    assert_eq!(
        manifest.get("pipeline").and_then(Node::text),
        Some("consumer")
    );
    assert_eq!(
        manifest.get("started_at").and_then(Node::text),
        Some("2026-11-06T11:35:45.628Z")
    );
    assert_eq!(
        completion.get("finished_at").and_then(Node::text),
        Some("2026-11-06T11:35:46.647Z")
    );
    let content = Cid::parse("bafkreif7gwanofb74dnqxnb5667ubt6si4johxp6om37lwtpzqslahruwm");
    let providers = completion
        .get("providers")
        .and_then(|node| node.get("head-node"));
    assert_eq!(
        providers,
        Some(&Node::List(vec![Node::Link(content.expect("a CID"))]))
    );

    let reader = "include { fromStore } from 'plugin/nf-blocks'\n\nworkflow {\n    \
                  channel.fromStore(run: 'latest', pipeline: 'consumer', output: 'aligned')\
                  .view { \"item: ${it}\" }\n}\n";
    let printed = run(folder, reader);
    let expected = "item: /bafkreif7gwanofb74dnqxnb5667ubt6si4johxp6om37lwtpzqslahruwm/A.bam";
    assert!(printed.contains(expected), "{printed}");
}
