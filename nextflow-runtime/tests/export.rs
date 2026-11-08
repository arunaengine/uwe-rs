//! The export of a built network keeps workflow calls, closures with their captures, data
//! sources as calls and what the build printed.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::export::{Kind, Network};
use serde_json::json;
use workflow_graph::Launch;

const PIPELINE: &str = r#"
process GREET {
    input:
    val name
    output:
    path "${name}.txt", emit: text
    val name, topic: names
    script:
    "echo ${name} > ${name}.txt"
}

workflow SUB {
    take:
    words
    main:
    def suffix = '!'
    GREET(words.map { word -> word + suffix })
    emit:
    text = GREET.out.text
}

workflow {
    log.info "starting"
    file("${launchDir}/built.txt").text = 'built'
    def temp = new File(launchDir.toString(), '.temp.txt')
    temp.text = 'copied'
    nextflow.extension.FilesEx.copyTo(temp.toPath(), "${launchDir}/copy.txt")
    temp.delete()
    files = channel.fromPath("${projectDir}/missing/*.csv", checkIfExists: true)
    SUB(channel.of('a', 'b'))
    channel.topic('names').mix(files).view()
}
"#;

/// A host in the folder of the test that fails the test when it prints.
struct Silent(String);

impl nextflow_evaluator::interpreter::Host for Silent {
    fn print(&mut self, text: &str) {
        panic!("the build printed {text:?}");
    }

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn export(directory: &std::path::Path) -> Network {
    std::fs::write(directory.join("main.nf"), PIPELINE).unwrap();
    let launch = Launch {
        target: directory.to_owned(),
        scratch: directory.join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Silent(directory.to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).unwrap();
    driver.export().expect("the network exports")
}

#[test]
fn keeps_workflow_calls() {
    let directory = tempfile::tempdir().unwrap();
    let network = export(directory.path());
    let names: Vec<&str> = network.scopes.iter().map(|scope| &*scope.name).collect();
    assert_eq!(names, ["", "SUB"]);
    let sub = &network.scopes[1];
    assert_eq!(sub.parent, Some(0));
    let [take] = sub.takes[..] else {
        panic!("one take: {:?}", sub.takes)
    };
    let of = network.nodes.iter().find(|node| node.outputs == [take]);
    let of = of.expect("the node of the take");
    assert!(
        matches!(&of.kind, Kind::Values { values, close: true } if *values == [json!("a"), json!("b")])
    );
    let process = network
        .nodes
        .iter()
        .find(|node| matches!(node.kind, Kind::Process { .. }));
    let process = process.expect("the process call");
    let Kind::Process {
        call,
        outputs,
        start,
        end,
        ..
    } = &process.kind
    else {
        unreachable!()
    };
    let definition = &PIPELINE[*start as usize..*end as usize];
    assert!(definition.starts_with("process GREET {"), "{definition}");
    assert_eq!(network.params, json!({}), "the params of the run");
    assert_eq!(
        (call.name.as_str(), call.process.as_str()),
        ("SUB:GREET", "GREET")
    );
    assert_eq!(process.scope, 1, "made in the call of SUB");
    assert_eq!(outputs[1].topic.as_deref(), Some("names"));
    assert_eq!(sub.emits, [(Some("text".to_owned()), process.outputs[0])]);
}

#[test]
fn keeps_closure_captures() {
    let directory = tempfile::tempdir().unwrap();
    let network = export(directory.path());
    let map = network.nodes.iter().find_map(|node| match &node.kind {
        Kind::Map(callback) => Some(callback),
        _ => None,
    });
    let map = map.expect("the map");
    assert_eq!(map.code, "{ word -> word + suffix }");
    let captured = [("suffix".to_owned(), json!("!"))];
    assert_eq!(map.captures, captured, "the captured local");
    assert_eq!(map.parameters, 1);
    assert_eq!(&PIPELINE[map.start as usize..map.end as usize], map.code);
}

#[test]
fn keeps_captured_closures() {
    let directory = tempfile::tempdir().unwrap();
    let script = "workflow {\n    def keep = { w -> w != 'b' }\n    \
                  channel.of(['a', 'b']).map { it.findAll(keep) }.view()\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Silent(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let network = Nextflow::new(&launch, settings).unwrap().export().unwrap();
    let map = network.nodes.iter().find_map(|node| match &node.kind {
        Kind::Map(callback) => Some(callback),
        _ => None,
    });
    let (name, inner) = &map.expect("the map").captures[0];
    assert_eq!(name, "keep");
    assert_eq!(inner["class"], "Closure");
    assert_eq!(inner["code"], "{ w -> w != 'b' }");
    let span = inner["start"].as_u64().unwrap() as usize..inner["end"].as_u64().unwrap() as usize;
    assert_eq!(&script[span], inner["code"]);
}

#[test]
fn defers_data_sources() {
    let directory = tempfile::tempdir().unwrap();
    let network = export(directory.path());
    let kinds: Vec<&Kind> = network.nodes.iter().map(|node| &node.kind).collect();
    let source = kinds.iter().find_map(|kind| match kind {
        Kind::Source { factory, arguments } => Some((factory, arguments)),
        _ => None,
    });
    let (factory, arguments) =
        source.expect("fromPath stays a call, so the missing file is no error");
    assert_eq!(factory, "fromPath");
    assert_eq!(arguments[0], json!({"checkIfExists": true}));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, Kind::Print(text) if text == "starting\n"))
    );
    let built = directory.path().join("built.txt");
    assert!(!built.exists(), "the build writes no file");
    let path = built.to_string_lossy();
    let written = |kind: &&&Kind| matches!(kind, Kind::Write { path: to, text } if *to == path && text == "built");
    assert!(kinds.iter().any(|kind| written(&kind)));
    let writes: Vec<(&str, &str)> = (kinds.iter())
        .filter_map(|kind| match kind {
            Kind::Write { path, text } => Some((path.rsplit('/').next().unwrap(), text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        writes,
        [("built.txt", "built"), ("copy.txt", "copied")],
        "the deleted file is gone"
    );
    let topic = network.nodes.iter().find(|node| node.name == "topic names");
    assert!(matches!(topic.map(|node| &node.kind), Some(Kind::Mix)));
    assert!(kinds.iter().any(|kind| matches!(kind, Kind::View { .. })));
    assert!(!kinds.iter().any(|kind| matches!(kind, Kind::Opaque(_))));
}

#[test]
fn reads_build_data() {
    let directory = tempfile::tempdir().unwrap();
    let script = "include { samplesheetToList } from 'plugin/nf-schema'\n\nworkflow {\n    \
                  def rows = samplesheetToList('sheet.csv', 'schema.json')\n    \
                  channel.fromList(rows.findAll { it[0] != 'b' }).view()\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    std::fs::write(
        directory.path().join("nextflow.config"),
        "plugins {\n    id 'nf-schema@2.5.1'\n}\n",
    )
    .unwrap();
    let schema = r#"{"type": "array", "items": {"type": "object",
        "properties": {"id": {"type": "string"}}}}"#;
    std::fs::write(directory.path().join("schema.json"), schema).unwrap();
    std::fs::write(directory.path().join("sheet.csv"), "id\na\nb\n").unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Silent(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).unwrap();
    let network = driver.export().unwrap();
    let values = network.nodes.iter().find_map(|node| match &node.kind {
        Kind::Values { values, .. } => Some(values.clone()),
        _ => None,
    });
    assert_eq!(values, Some(vec![json!(["a"])]), "the build read the sheet");
    let read: Vec<&str> = (network.reads.iter())
        .map(|(location, _)| location.rsplit('/').next().unwrap())
        .collect();
    assert!(read.contains(&"sheet.csv"), "{read:?}");
    let sheet = network
        .reads
        .iter()
        .find(|(location, _)| location.ends_with("sheet.csv"));
    let digest = "sha256:55fc01feec80df5521dde64da128cf7c9a69b3dad5d275b5008f6ba671f8c915";
    assert_eq!(sheet.map(|(_, known)| known.as_str()), Some(digest));
}

#[test]
fn keeps_binary_writes() {
    let directory = tempfile::tempdir().unwrap();
    let script =
        "workflow {\n    file(\"${projectDir}/in.gz\").copyTo(\"${launchDir}/out.gz\")\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    std::fs::write(
        directory.path().join("in.gz"),
        [0x1f, 0x8b, 0x08, 0xff, 0xfe],
    )
    .unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Silent(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let network = Nextflow::new(&launch, settings).unwrap().export().unwrap();
    let kinds: Vec<&Kind> = network.nodes.iter().map(|node| &node.kind).collect();
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, Kind::Opaque(why) if why.contains("no text"))),
        "a binary file keeps the pipeline native: {kinds:?}"
    );
    assert!(!kinds.iter().any(|kind| matches!(kind, Kind::Write { .. })));
}

#[test]
fn pins_file_checks() {
    let directory = tempfile::tempdir().unwrap();
    let script = "workflow {\n    def found = file(\"${projectDir}/index\").exists()\n    \
                  def names = file(\"${projectDir}/data\").list()\n    \
                  channel.of(found, names.size()).view()\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).unwrap();
    std::fs::create_dir(directory.path().join("data")).unwrap();
    std::fs::write(directory.path().join("data").join("a.txt"), "a").unwrap();
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Silent(directory.path().to_string_lossy().into_owned())),
        ..Settings::default()
    };
    let network = Nextflow::new(&launch, settings).unwrap().export().unwrap();
    let pinned: Vec<&str> = (network.reads.iter())
        .map(|(location, _)| location.rsplit('/').next().unwrap())
        .collect();
    assert!(pinned.contains(&"index#metadata"), "{pinned:?}");
    assert!(pinned.contains(&"data#listing"), "{pinned:?}");
}
