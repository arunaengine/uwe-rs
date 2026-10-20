//! Loads scripts with includes from files and checks the errors of bad includes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::Loader;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates directories");
    std::fs::write(path, text).expect("writes the script");
}

#[test]
fn links_included_scripts() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(root.path(), "lib.nf", "def twice(x) { x * 2 }\n");
    write(
        root.path(),
        "subworkflows/show/main.nf",
        "include { twice as times2 } from '../../lib'\n\
         workflow SHOW {\n    take:\n    ch\n    main:\n    ch.map { v -> times2(v) }.view()\n}\n",
    );
    write(
        root.path(),
        "main.nf",
        "include { SHOW } from './subworkflows/show'\nworkflow {\n    SHOW(channel.of(1, 2))\n    println(moduleDir.name)\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    let directory = root
        .path()
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    assert_eq!(output.0, format!("{directory}\n2\n4\n"));
}

#[test]
fn bad_includes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "a.nf",
        "include { B } from './b'\nworkflow A {\n    main:\n    B()\n}\n",
    );
    write(
        root.path(),
        "b.nf",
        "include { A } from './a'\nworkflow B {\n    main:\n    A()\n}\n",
    );
    write(
        root.path(),
        "missing.nf",
        "include { X } from './nowhere'\n",
    );
    write(root.path(), "c.nf", "def c() { 1 }\n");
    write(root.path(), "undefined.nf", "include { X } from './c'\n");
    write(root.path(), "broken.nf", "def f( {\n");
    let error = |name: &str| {
        let path = root.path().join(name);
        let error = Loader::default().load(&path).expect_err("fails");
        error
            .messages
            .join("\n")
            .replace(&root.path().display().to_string(), "<root>")
    };
    assert!(
        error("a.nf").contains("<root>/a.nf: the script includes itself"),
        "{}",
        error("a.nf")
    );
    assert_eq!(
        error("missing.nf"),
        "<root>/missing.nf:1:11: the included file <root>/nowhere.nf does not exist"
    );
    assert_eq!(
        error("undefined.nf"),
        "<root>/undefined.nf:1:11: Included name 'X' is not defined in module '<root>/c.nf'"
    );
    assert!(
        error("broken.nf").starts_with("<root>/broken.nf:1:"),
        "{}",
        error("broken.nf")
    );
}

/// Nextflow's strict parser checks the number of arguments of calls when it loads a script;
/// the legacy parser does not, and a call only fails when it runs (decision 14).
#[test]
fn requires_input_arguments() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "lib.nf",
        "process HELLO {\n    input:\n    val x\n    script:\n    \"echo ${x}\"\n}\n",
    );
    write(
        root.path(),
        "main.nf",
        "include { HELLO as SAY } from './lib'\n\
         workflow SUB {\n    take:\n    a\n    b\n    main:\n    println(a)\n}\n\
         workflow {\n    if (params.say) { SAY('a', 'b') }\n    SUB('x')\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let error = nextflow_runtime::run(&module, &mut Output::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with("main.nf:11:5: IllegalArgumentException: Workflow `SUB` declares 2 input channels but 1 were given"),
        "{error}"
    );
}

#[test]
fn calls_included_plugins() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "include { samplesheetToList as toList } from 'plugin/nf-schema'\n\
         workflow {\n    toList('sheet.csv', 'schema.json')\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let error = nextflow_runtime::run(&module, &mut Output::default()).unwrap_err();
    let error = error.to_string();
    assert!(
        error.ends_with("main.nf:3:5: NoSuchFileException: /schema.json"),
        "{error}"
    );
    write(
        root.path(),
        "other.nf",
        "include { fromCatalogue } from 'plugin/nf-unknown'\n\
         include { nothing } from 'plugin/nf-schema'\nworkflow {\n}\n",
    );
    let error = Loader::default()
        .load(&root.path().join("other.nf"))
        .unwrap_err()
        .to_string();
    let lines: Vec<&str> = error
        .lines()
        .map(|line| line.rsplit("other.nf:").next().unwrap_or(line))
        .collect();
    assert_eq!(
        lines,
        [
            "1:11: Plugins are not supported yet: plugin/nf-unknown",
            "2:11: `nothing` is not a function of the plugin `nf-schema`"
        ]
    );
}

#[test]
fn creates_plugin_channels() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let dir = root.path().display();
    write(
        root.path(),
        "schema.json",
        &format!(
            "{{\"definitions\": {{\"io\": {{\"properties\": {{\"input\": \
             {{\"type\": \"string\", \"schema\": \"{dir}/sheet.json\"}}}}}}}}}}"
        ),
    );
    write(
        root.path(),
        "sheet.json",
        "{\"items\": {\"properties\": {\"sample\": {\"type\": \"string\", \"meta\": [\"id\"]}, \
         \"n\": {\"type\": \"integer\"}}}}",
    );
    write(root.path(), "sheet.csv", "sample,n\nA,1\nB,2\n");
    write(
        root.path(),
        "main.nf",
        &format!(
            "include {{ fromSamplesheet }} from 'plugin/nf-validation'\n\
             params.input = '{dir}/sheet.csv'\n\
             workflow {{\n    Channel.fromSamplesheet('input', parameters_schema: '{dir}/schema.json').view()\n}}\n"
        ),
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "[[id:A], 1]\n[[id:B], 2]\n");
}

#[test]
fn loads_plugin_scripts() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "include { getWorkflowVersion; checkConfigProvided; softwareVersionsToYAML } from 'plugin/nf-core-utils'\n\
         workflow {\n\
             println(getWorkflowVersion('1.0', 'abcdef123'))\n\
             println(checkConfigProvided())\n\
             def versions = channel.of(['A:FASTQC', 'fastqc', '0.12.1'], \"B:\\n  x:y: '2'\\n\")\n\
             softwareVersionsToYAML(softwareVersions: versions, nextflowVersion: '26.04.6').view()\n\
         }\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(
        output.0,
        "v1.0-gabcdef1\nfalse\nB:\n  y: '2'\nFASTQC:\n  fastqc: 0.12.1\n\
         Workflow:\n    unknown: \n    Nextflow: 26.04.6\n"
    );
    write(
        root.path(),
        "email.nf",
        "include { completionEmail } from 'plugin/nf-core-utils'\n\
         workflow {\n    completionEmail([:], 'a@b.c', null, false, 'out', true, [])\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("email.nf"))
        .expect("loads");
    let error = nextflow_runtime::run(&module, &mut Output::default()).unwrap_err();
    let error = error.to_string();
    assert!(
        error.ends_with(
            "email.nf:3:5: Not supported yet: the function `completionEmail` of the plugin `nf-core-utils`"
        ),
        "{error}"
    );
}

#[test]
fn renders_task_templates() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "templates/hello.sh",
        "echo ${x} $task.process\n",
    );
    write(
        root.path(),
        "main.nf",
        "process P {\n    input:\n    val x\n    script:\n    template 'hello.sh'\n}\n\
         workflow {\n    P(channel.of('a'))\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let options = nextflow_runtime::Options::default();
    let (_, requests) =
        nextflow_runtime::Run::start(&module, &options, &mut Output::default()).expect("starts");
    assert_eq!(requests[0].script, "#!/bin/bash -ue\necho a P\n");
}

/// Nextflow 26.04.6 calls included functions, and functions called from a workflow or
/// process, by reflection, which wraps what they throw in an `InvocationTargetException`.
#[test]
fn wraps_reflective_calls() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "mod.nf",
        "def mboom() { throw new IllegalStateException('m') }\ndef mcall() { mboom() }\n",
    );
    write(
        root.path(),
        "main.nf",
        r#"include { mboom; mcall } from './mod'
include { readStructure } from 'plugin/nf-fgbio'
def boom() { throw new RuntimeException('rt') }
def check() { assert false }
def direct() { try { boom() } catch (Exception e) { println "a ${e.getClass().name}" } }
def callm() { mboom() }
def clos() { def c = { boom() }; try { c.call() } catch (Exception e) { println "g ${e.getClass().name}" } }
workflow {
    direct()
    try { boom() } catch (Exception e) { println "b ${e.getClass().name} ${e.message} ${e.cause.getClass().name}" }
    try { callm() } catch (Exception e) { println "c ${e.cause.cause.getClass().name}" }
    try { mcall() } catch (Exception e) { println "d ${e.cause.getClass().name}" }
    def c = { boom() }
    try { c.call() } catch (Exception e) { println "e ${e.getClass().name}" }
    clos()
    try { check() } catch (Exception e) { println "f ${e.getClass().name}" }
    try { readStructure('10B20C+T') } catch (Exception e) { println "h ${e.cause.getClass().name}" }
    try { channel.of(1).map { v -> boom() } } catch (Exception e) { println "never" }
}
"#,
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    let error = nextflow_runtime::run(&module, &mut output).unwrap_err();
    let wrapper = "java.lang.reflect.InvocationTargetException";
    let expected = [
        "a java.lang.RuntimeException".to_owned(),
        format!("b {wrapper} null java.lang.RuntimeException"),
        "c java.lang.IllegalStateException".into(),
        "d java.lang.IllegalStateException".into(),
        format!("e {wrapper}"),
        "g java.lang.RuntimeException".into(),
        format!("f {wrapper}"),
        "h java.lang.IllegalArgumentException".into(),
    ];
    assert_eq!(output.0.lines().collect::<Vec<_>>(), expected);
    let error = error.to_string();
    assert!(
        error.ends_with("main.nf:3:14: RuntimeException: rt"),
        "{error}"
    );
}
