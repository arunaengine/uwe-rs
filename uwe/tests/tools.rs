//! Composes a CWL tool, a Galaxy tool and a WDL task in one graph that maps over a list, checks
//! it, lowers it into wir and runs it on the simulator, also as a `.uwe.yaml` file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use tool_definition::{Fit, PortKind, SourceKind};
use uwe::tools::{ToolLibrary, composed, lower};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Launch, Simulator, execute};

use super::common;

const SORT: &str = r#"#!/usr/bin/env cwl-runner
cwlVersion: v1.2
class: CommandLineTool
label: Sort lines
$namespaces:
  edam: http://edamontology.org/
hints:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [sort]
inputs:
  text:
    type: File
    format: edam:format_2330
    inputBinding:
      position: 1
stdout: sorted.txt
outputs:
  sorted:
    type: stdout
    format: edam:format_2330
"#;

const COUNT: &str = r#"<tool id="count_lines" name="Count lines" version="1.0" profile="22.05">
    <command detect_errors="exit_code"><![CDATA[
        wc -l < '$input' > '$output'
    ]]></command>
    <inputs>
        <param name="input" type="data" format="txt" label="Lines to count"/>
    </inputs>
    <outputs>
        <data name="output" format="txt"/>
    </outputs>
</tool>
"#;

const SUMMARY: &str = r#"version 1.1

task summarize {
  input {
    Array[File] counts
  }
  command <<<
    cat ~{sep(" ", counts)} > report.txt
  >>>
  output {
    File report = "report.txt"
  }
  runtime {
    container: "debian:stable-slim"
  }
}
"#;

const WORKFLOW: &str = r#"# Sorts and counts the lines of each file, and sums up the counts.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
uwe_version: "1"
name: polyglot
description: Sorts and counts the lines of each file, and sums up the counts.
tools:
  - tools
inputs:
  texts:
    type: file
    list: true
    format: format_2330
    description: The files to count.
steps:
  sort:
    tool: sort
    in:
      text: texts
  count:
    tool: count_lines
    in:
      input: sort/sorted
  summary:
    tool: summarize
    in:
      counts: count/output
outputs:
  report:
    from: summary/report
    description: The counts of all files.
"#;

const READS: &str = r#"version 1.1

task count_reads {
  input {
    File reads
  }
  command <<<
    wc -l < ~{reads} > count.txt
  >>>
  output {
    File count = "count.txt"
  }
  runtime {
    container: "debian:stable-slim"
  }
}
"#;

const FASTQ: &str = r#"uwe_version: "1"
name: reads
tools:
  - tools
inputs:
  reads:
    type: file
    format: format_1930
steps:
  count:
    tool: count_reads
    in:
      reads: reads
outputs:
  count:
    from: count/count
"#;

/// A folder with the three tools below `tools/`, the composed workflow and two text files.
fn project(root: &Path) {
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).expect("makes the tool folder");
    std::fs::write(tools.join("sort.cwl"), SORT).expect("writes the CWL tool");
    std::fs::write(tools.join("count.xml"), COUNT).expect("writes the Galaxy tool");
    std::fs::write(tools.join("summary.wdl"), SUMMARY).expect("writes the WDL task");
    std::fs::write(root.join("polyglot.uwe.yaml"), WORKFLOW).expect("writes the workflow");
    std::fs::write(root.join("a.txt"), "b\na\n").expect("writes a text");
    std::fs::write(root.join("b.txt"), "c\n").expect("writes a text");
    // The CWL tool takes plain text only, so the files say that they are.
    let format = "http://edamontology.org/format_2330";
    let texts = json!({"texts": [
        {"class": "File", "location": "a.txt", "format": format},
        {"class": "File", "location": "b.txt", "format": format}
    ]});
    std::fs::write(root.join("inputs.json"), texts.to_string()).expect("writes the inputs");
}

#[test]
fn extracts_three_languages() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    project(folder.path());
    let mut library = ToolLibrary::new();
    assert_eq!(library.scan(&folder.path().join("tools")), 3);
    let kind = |id: &str| {
        let entry = library.get(id, None).expect("a tool");
        entry.definition.source.as_ref().map(|source| source.kind)
    };
    assert_eq!(kind("sort"), Some(SourceKind::Cwl));
    assert_eq!(kind("count_lines"), Some(SourceKind::Galaxy));
    assert_eq!(kind("summarize"), Some(SourceKind::Wdl));
    let count = &library.get("count_lines", None).expect("a tool").definition;
    assert_eq!(
        count.inputs[0].formats,
        ["format_2330"],
        "txt is plain text"
    );
    let sort = &library.get("sort", None).expect("a tool").definition;
    assert_eq!(sort.inputs[0].formats, ["format_2330"]);
    assert_eq!(
        sort.image.as_ref().map(|image| image.name.as_str()),
        Some("debian:stable-slim")
    );
    let summary = &library.get("summarize", None).expect("a tool").definition;
    assert_eq!(summary.inputs[0].kind, PortKind::File);
    assert_eq!(
        summary.inputs[0].cardinality,
        tool_definition::Cardinality::List
    );
}

#[test]
fn runs_polyglot_graph() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    project(root);
    let mut library = ToolLibrary::new();
    let workflow =
        composed::load(&root.join("polyglot.uwe.yaml"), &mut library).expect("a workflow");
    let graph = workflow.graph_with(&library.definitions());
    let report = library.check_graph(&graph);
    assert_ne!(report.fit, Fit::DoesNotFit, "{report:?}");
    let scatter: Vec<bool> = report.nodes.iter().map(|node| node.scatter).collect();
    assert_eq!(
        scatter,
        [true, true, false],
        "sort and count map, summary gathers"
    );
    let lowered = lower(&workflow, &library, (root, &root.join("generated"))).expect("lowers");
    let document = &lowered.document;
    wir::verify(document).expect("the document verifies");
    let entry = &document.graphs[0];
    let kinds: Vec<&str> = (entry.ops.iter())
        .map(|op| match &op.kind {
            wir::Kind::Map(_) => "map",
            wir::Kind::Native(_) => "native",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["map", "map", "native"]);
    let languages: Vec<wir::Language> = document.sources.iter().map(|s| s.language).collect();
    assert_eq!(
        languages,
        [
            wir::Language::Cwl,
            wir::Language::Galaxy,
            wir::Language::Wdl
        ]
    );
    assert_eq!(
        lowered.annotations["texts"]["description"],
        "The files to count."
    );
    let text = serde_json::to_string_pretty(document).expect("JSON");
    std::fs::write(root.join("polyglot.wir.json"), text).expect("writes the document");
    let launch = Launch {
        target: root.join("polyglot.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        simulated: true,
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).expect("a driver");
    let mut simulator = Simulator::new(root.join("simulator"));
    let graph = execute("polyglot", &mut driver, &mut simulator)
        .into_result()
        .expect("the run succeeds");
    let names: Vec<&str> = graph
        .nodes
        .iter()
        .map(|node| node.task.name.as_str())
        .collect();
    let counted = |part: &str| names.iter().filter(|name| name.contains(part)).count();
    assert_eq!(counted("sort"), 2, "one sort per file: {names:?}");
    assert_eq!(counted("count"), 2, "one count per file: {names:?}");
    assert_eq!(counted("summarize"), 1, "{names:?}");
    let outputs = driver.outputs().expect("outputs");
    assert_eq!(outputs["report"]["class"], "File");
}

const MODULE: &str = r#"process COUNT {
    container "${ workflow.containerEngine == 'singularity' ? 'https://depot.galaxyproject.org/singularity/coreutils:9.5' : 'quay.io/biocontainers/coreutils:9.5' }"

    input:
    tuple val(meta), path(text)

    output:
    tuple val(meta), path("*.count"), emit: counts
    path "versions.yml", emit: versions

    script:
    """
    wc -l < ${text} > ${meta.id}.count
    echo 'COUNT: 1' > versions.yml
    """
}
"#;

const META: &str = r#"name: count
description: Counts the lines of a text file
keywords: [count, lines]
input:
  - - meta:
        type: map
        description: The sample
    - text:
        type: file
        description: The text to count
        ontologies:
          - edam: http://edamontology.org/format_2330
output:
  counts:
    - - meta:
          type: map
      - "*.count":
          type: file
          description: The line count
          ontologies:
            - edam: http://edamontology.org/format_2330
authors: ["@uwe"]
"#;

#[test]
fn lowers_nfcore_modules() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let module = root.join("modules/count");
    std::fs::create_dir_all(&module).expect("makes the module folder");
    std::fs::write(module.join("main.nf"), MODULE).expect("writes the module");
    std::fs::write(module.join("meta.yml"), META).expect("writes its meta.yml");
    let mut library = ToolLibrary::new();
    assert_eq!(library.scan(root), 1, "{:?}", library.skipped());
    let count = &library.get("count", None).expect("the module").definition;
    assert_eq!(
        count.source.as_ref().map(|s| s.kind),
        Some(SourceKind::NfCore)
    );
    assert_eq!(count.keywords, ["count", "lines"]);
    assert_eq!(count.inputs[0].name, "text");
    assert_eq!(count.inputs[0].formats, ["format_2330"]);
    assert_eq!(count.outputs[0].name, "counts");
    let image = count.image.as_ref().map(|image| image.name.as_str());
    assert_eq!(image, Some("quay.io/biocontainers/coreutils:9.5"));
    let text = "uwe_version: \"1\"\nname: counting\ninputs:\n  text:\n    type: file\n\
                steps:\n  count:\n    tool: count\n    in:\n      text: text\n";
    let mut workflow = composed::parse(text).expect("a workflow");
    workflow
        .resolve(&mut library, None)
        .expect("the tools are known");
    let generated = root.join("generated");
    let lowered = lower(&workflow, &library, (root, &generated)).expect("lowers");
    wir::verify(&lowered.document).expect("verifies");
    let (file, script) = &lowered.files[0];
    assert!(file.starts_with(&generated));
    assert!(
        script.contains("COUNT(channel.of([[id: 'node'], file(params['text'])]))"),
        "{script}"
    );
    assert!(
        script.contains("counts = COUNT.out.counts.map { it[1] }.first()"),
        "{script}"
    );
    let outputs = &lowered.document.graphs[0].outputs;
    let names: Vec<&str> = outputs.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(names, ["count.counts", "count.versions"]);
    for (file, text) in &lowered.files {
        std::fs::create_dir_all(file.parent().expect("a folder")).expect("makes the folder");
        std::fs::write(file, text).expect("writes the entry script");
    }
    let text = serde_json::to_string(&lowered.document).expect("JSON");
    std::fs::write(root.join("counting.wir.json"), text).expect("writes the document");
    std::fs::write(root.join("a.txt"), "a\nb\n").expect("writes a text");
    let inputs = json!({"text": {"class": "File", "location": "a.txt"}});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).expect("writes the inputs");
    let launch = Launch {
        target: root.join("counting.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        simulated: true,
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).expect("a driver");
    let mut simulator = Simulator::new(root.join("simulator"));
    let graph = execute("counting", &mut driver, &mut simulator)
        .into_result()
        .expect("the run succeeds");
    let names: Vec<&str> = graph
        .nodes
        .iter()
        .map(|node| node.task.name.as_str())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

const PLAIN: &str = r#"process COUNT {
    input:
    path text

    output:
    path "*.count"
    path "log.txt", emit: log

    script:
    """
    wc -l < ${text} > text.count
    """
}
"#;

#[test]
fn digests_name_wrappers() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let generated = root.join("generated");
    let text = "uwe_version: \"1\"\nname: counting\ninputs:\n  text:\n    type: file\n\
                steps:\n  count:\n    tool: count\n    in:\n      text: text\n";
    let mut lowered = Vec::new();
    for (name, module) in [("it's", MODULE), ("plain", PLAIN)] {
        let folder = root.join("modules").join(name);
        std::fs::create_dir_all(&folder).expect("makes the module folder");
        std::fs::write(folder.join("main.nf"), module).expect("writes the module");
        let mut library = ToolLibrary::new();
        library.add_file(&folder.join("main.nf")).expect("a tool");
        let mut workflow = composed::parse(text).expect("a workflow");
        workflow
            .resolve(&mut library, None)
            .expect("the tools are known");
        lowered.push(lower(&workflow, &library, (root, &generated)).expect("lowers"));
    }
    let files: Vec<&Path> = lowered.iter().map(|one| one.files[0].0.as_path()).collect();
    assert_ne!(files[0], files[1], "two workflows with one step name");
    for one in &lowered {
        let (file, script) = &one.files[0];
        let digest = task_model::Digest::sha256(script.as_bytes());
        let name = format!("count-{}.nf", &digest.hex()[..12]);
        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some(name.as_str())
        );
        let source = &one.document.sources[0];
        assert_eq!(source.location, file.to_string_lossy());
        assert_eq!(source.digest, Some(format!("sha256:{}", digest.hex())));
    }
    let quoted = &lowered[0].files[0].1;
    assert!(quoted.contains("/it\\'s/main.nf'"), "{quoted}");
    let plain = &lowered[1].files[0].1;
    assert!(plain.contains("output_1 = COUNT.out[0].first()"), "{plain}");
    assert!(plain.contains("log = COUNT.out.log.first()"), "{plain}");
}

#[test]
fn keeps_sources_apart() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).expect("makes the tool folder");
    std::fs::write(tools.join("trim galore.cwl"), SORT).expect("writes a tool");
    std::fs::write(tools.join("trim_galore.cwl"), SORT).expect("writes a tool");
    let text = "uwe_version: \"1\"\nname: trims\ntools:\n  - tools\ninputs:\n  text:\n\
                \x20   type: file\n    format: format_2330\nsteps:\n  one:\n    tool: trim galore\n\
                \x20   in:\n      text: text\n  two:\n    tool: trim_galore\n    in:\n      text: text\n";
    std::fs::write(root.join("trims.uwe.yaml"), text).expect("writes the workflow");
    let mut library = ToolLibrary::new();
    let workflow = composed::load(&root.join("trims.uwe.yaml"), &mut library).expect("a workflow");
    let lowered = lower(&workflow, &library, (root, &root.join("generated"))).expect("lowers");
    let sources = &lowered.document.sources;
    assert_eq!(sources.len(), 2, "{sources:?}");
    assert_ne!(sources[0].id, sources[1].id);
    assert_ne!(sources[0].location, sources[1].location);
}

const SPLIT: &str = r#"cwlVersion: v1.2
class: CommandLineTool
baseCommand: [split, -l, "1"]
inputs:
  text:
    type: File
    inputBinding:
      position: 1
outputs:
  parts:
    type: File[]
    outputBinding:
      glob: "x*"
"#;

const JOIN: &str = r#"cwlVersion: v1.2
class: CommandLineTool
baseCommand: [cat]
inputs:
  parts:
    type: File[]
    inputBinding:
      position: 1
stdout: joined.txt
outputs:
  joined:
    type: stdout
"#;

#[test]
fn maps_sample_lists() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).expect("makes the tool folder");
    std::fs::write(tools.join("split.cwl"), SPLIT).expect("writes a tool");
    std::fs::write(tools.join("join.cwl"), JOIN).expect("writes a tool");
    let text = "uwe_version: \"1\"\nname: samples\ntools:\n  - tools\ninputs:\n  texts:\n\
                \x20   type: file\n    list: true\nsteps:\n  split:\n    tool: split\n\
                \x20   in:\n      text: texts\n  join:\n    tool: join\n    in:\n      parts: split/parts\n";
    std::fs::write(root.join("samples.uwe.yaml"), text).expect("writes the workflow");
    let mut library = ToolLibrary::new();
    let workflow =
        composed::load(&root.join("samples.uwe.yaml"), &mut library).expect("a workflow");
    let report = library.check_graph(&workflow.graph_with(&library.definitions()));
    let over: Vec<Vec<String>> = report.nodes.iter().map(|node| node.over.clone()).collect();
    assert_eq!(over, [["text"], ["parts"]], "a list per sample maps too");
    let lowered = lower(&workflow, &library, (root, &root.join("generated"))).expect("lowers");
    let maps: Vec<Vec<String>> = (lowered.document.graphs[0].ops.iter())
        .map(|op| match &op.kind {
            wir::Kind::Map(map) => map.over.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(maps, [["text"], ["parts"]]);
}

const STATS: &str = r#"cwlVersion: v1.2
class: CommandLineTool
requirements:
  InlineJavascriptRequirement: {}
baseCommand: ["true"]
inputs:
  text:
    type: File
    inputBinding:
      position: 1
outputs:
  extra:
    type: File?
    outputBinding:
      glob: extra.txt
  summary:
    type:
      type: record
      fields:
        lines: int
    outputBinding:
      outputEval: '$({"lines": 1})'
"#;

#[test]
fn keeps_output_shapes() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).expect("makes the tool folder");
    std::fs::write(tools.join("stats.cwl"), STATS).expect("writes a tool");
    let text = "uwe_version: \"1\"\nname: stats\ntools:\n  - tools\ninputs:\n  text:\n\
                \x20   type: file\nsteps:\n  stats:\n    tool: stats\n    in:\n      text: text\n";
    std::fs::write(root.join("stats.uwe.yaml"), text).expect("writes the workflow");
    let mut library = ToolLibrary::new();
    assert_eq!(library.scan(&tools), 1, "{:?}", library.skipped());
    let workflow = composed::load(&root.join("stats.uwe.yaml"), &mut library).expect("a workflow");
    let stats = &library.get("stats", None).expect("the tool").definition;
    assert!(stats.outputs[0].optional && !stats.outputs[1].optional);
    let lowered = lower(&workflow, &library, (root, &root.join("generated"))).expect("lowers");
    let outputs = &lowered.document.graphs[0].outputs;
    assert!(
        matches!(outputs[0].r#type, wir::Type::Optional(_)),
        "{outputs:?}"
    );
    let lines = wir::Field {
        name: "lines".into(),
        r#type: wir::Type::Optional(Box::new(wir::Type::Int)),
    };
    assert_eq!(
        outputs[1].r#type,
        wir::Type::Record(vec![lines]),
        "{outputs:?}"
    );
}

/// Runs the binary in `directory` with the cache folder `cache`; its exit code and what it
/// printed on standard output and standard error.
fn uwe(directory: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (
        output.status.code().unwrap_or(-1),
        text(&output.stdout),
        text(&output.stderr),
    )
}

#[test]
fn runs_composed_files() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    project(root);
    let (code, printed, errors) = uwe(root, &["lower", "polyglot.uwe.yaml"]);
    assert_eq!(code, 0, "{errors}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["entry"], "polyglot");
    assert_eq!(document["sources"][0]["location"], "tools/sort.cwl");
    let (code, printed, errors) = uwe(root, &["inputs", "polyglot.uwe.yaml"]);
    assert_eq!(code, 0, "{errors}");
    let schema: Json = serde_json::from_str(&printed).expect("prints JSON");
    let texts = &schema["properties"]["texts"];
    assert_eq!(texts["items"]["format"], "file-path");
    assert_eq!(texts["description"], "The files to count.");
    assert_eq!(
        texts["fileFormat"],
        json!(["http://edamontology.org/format_2330"])
    );
    let (code, printed, errors) = uwe(root, &["outputs", "polyglot.uwe.yaml"]);
    assert_eq!(code, 0, "{errors}");
    assert!(printed.contains("\"report\""), "{printed}");
    let words = [
        "run",
        "-executor",
        "simulator",
        "polyglot.uwe.yaml",
        "-inputs",
        "inputs.json",
    ];
    let (code, printed, errors) = uwe(root, &words);
    assert_eq!(code, 0, "{printed}\n{errors}");
    let items = &document["graphs"][0]["inputs"][0]["type"]["array"]["items"];
    assert_eq!(
        items["file"]["format"],
        "http://edamontology.org/format_2330"
    );
    // Plain paths take the format of the workflow input, which the CWL tool checks; a file of
    // another format does not fit.
    let plain = json!({"texts": ["a.txt", "b.txt"]});
    std::fs::write(root.join("plain.json"), plain.to_string()).expect("writes the inputs");
    let words = [
        "run",
        "-executor",
        "simulator",
        "polyglot.uwe.yaml",
        "-inputs",
        "plain.json",
    ];
    let (code, printed, errors) = uwe(root, &words);
    assert_eq!(code, 0, "{printed}\n{errors}");
    let bam = "http://edamontology.org/format_2572";
    let other = json!({"texts": [{"class": "File", "location": "a.txt", "format": bam}]});
    std::fs::write(root.join("other.json"), other.to_string()).expect("writes the inputs");
    let words = [
        "run",
        "-executor",
        "simulator",
        "polyglot.uwe.yaml",
        "-inputs",
        "other.json",
    ];
    let (code, _, errors) = uwe(root, &words);
    assert_ne!(code, 0, "a file of another format does not fit");
    assert!(errors.contains("does not fit its type"), "{errors}");
    let check = ["check", "polyglot.uwe.yaml", "-inputs", "inputs.json"];
    let (code, printed, errors) = uwe(root, &check);
    assert_eq!(code, 0, "{printed}\n{errors}");
    let graph = [
        "graph",
        "polyglot.uwe.yaml",
        "-inputs",
        "inputs.json",
        "-format",
        "summary",
    ];
    let (code, printed, errors) = uwe(root, &graph);
    assert_eq!(code, 0, "{printed}\n{errors}");
    let missing = ["run", "-executor", "simulator", "polyglot.uwe.yaml"];
    let (code, _, errors) = uwe(root, &missing);
    assert_ne!(code, 0, "a run without its required input stops");
    assert!(errors.contains("texts"), "{errors}");
}

#[test]
fn runs_galaxy_tools() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    project(root);
    std::fs::write(
        root.join("count.json"),
        json!({"input": "a.txt"}).to_string(),
    )
    .expect("writes the job");
    let (code, printed, errors) = uwe(root, &["inputs", "tools/count.xml"]);
    assert_eq!(code, 0, "{errors}");
    let schema: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(schema["properties"]["input"]["format"], "file-path");
    let words = [
        "run",
        "-executor",
        "simulator",
        "tools/count.xml",
        "-inputs",
        "count.json",
    ];
    let (code, printed, errors) = uwe(root, &words);
    assert_eq!(code, 0, "{printed}\n{errors}");
    let (code, printed, errors) = uwe(root, &["lower", "tools/count.xml"]);
    assert_eq!(code, 0, "{errors}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["sources"][0]["language"], "galaxy");
    assert_eq!(
        document["graphs"][0]["ops"][0]["kind"]["native"]["language"],
        "galaxy"
    );
}

#[test]
fn fits_format_kinds() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::create_dir_all(root.join("tools")).expect("makes the tool folder");
    std::fs::write(root.join("tools/reads.wdl"), READS).expect("writes the WDL task");
    std::fs::write(root.join("reads.uwe.yaml"), FASTQ).expect("writes the workflow");
    std::fs::write(root.join("r.fq"), "@r\nACGT\n+\nIIII\n").expect("writes the reads");
    // FASTQ-sanger fits the FASTQ input, as the check of the graph says; BAM does not.
    for (format, fits) in [("format_1932", true), ("format_2572", false)] {
        let format = format!("http://edamontology.org/{format}");
        let file = json!({"class": "File", "location": "r.fq", "format": format});
        let inputs = json!({"reads": file});
        std::fs::write(root.join("inputs.json"), inputs.to_string()).expect("writes the inputs");
        let words = [
            "run",
            "-executor",
            "simulator",
            "reads.uwe.yaml",
            "-inputs",
            "inputs.json",
        ];
        let (code, printed, errors) = uwe(root, &words);
        assert_eq!(code == 0, fits, "{format}: {printed}\n{errors}");
        assert!(fits || errors.contains("does not fit its type"), "{errors}");
    }
}
