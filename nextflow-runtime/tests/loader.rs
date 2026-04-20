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
fn includes_link_to_their_scripts() {
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
        "<root>/missing.nf:1:11: Invalid include source: '<root>/nowhere.nf'"
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
fn calls_need_as_many_arguments_as_inputs_when_they_run() {
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
fn plugin_includes_call_the_plugin() {
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
fn plugin_factories_make_channels() {
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
fn plugin_scripts_port_plugin_functions() {
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
fn process_templates_render_with_task_variables() {
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
