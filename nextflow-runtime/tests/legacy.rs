//! Scripts of legacy syntax, which strict syntax rejects and iunctus runs with the meaning of
//! the legacy parser (decision 14).
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
    std::fs::write(root.join(name), text).expect("writes the script");
}

#[test]
fn statements_next_to_declarations_run_when_the_script_loads() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "def greeting = 'hello'
if (greeting) {
    names = ['a', 'b']
}
include { SHOW } from './show'
println \"main: ${greeting}\"
def shout(text) { text.toUpperCase() + names.size() }
workflow {
    SHOW(channel.fromList(names))
    println(shout(greeting))
}
",
    );
    write(
        root.path(),
        "show.nf",
        "def prefix = 'item'
println 'show loaded'
workflow SHOW {
    take:
    ch
    main:
    ch.map { v -> \"${prefix} ${v}\" }.view()
}
",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(
        output.0,
        "main: hello\nshow loaded\nHELLO2\nitem a\nitem b\n"
    );
}

/// A `lib/` class like the `NfcoreSchema` of older nf-core pipelines, which reads a schema
/// with `org.json`, drops an ignored param from it and validates params with everit. The
/// violations are those of everit 1.14.1, which
/// `nextflow-evaluator/tests/fixtures/oracle/org_json/schema.groovy` prints.
#[test]
fn lib_classes_validate_with_org_json_and_everit() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(root.path().join("lib")).expect("creates lib");
    write(
        root.path(),
        "lib/Check.groovy",
        r#"import groovy.json.JsonBuilder
import org.json.JSONObject
import org.json.JSONTokener
import org.everit.json.schema.Schema
import org.everit.json.schema.loader.SchemaLoader
import org.everit.json.schema.ValidationException

class Check {
    static List errors(String text, Map params) {
        JSONObject raw = new JSONObject(new JSONTokener(text))
        raw.definitions.each { definition ->
            for (key in definition.keySet()) {
                if (definition[key].get("properties").keySet().contains('skip')) {
                    definition[key].get("properties").remove('skip')
                }
            }
        }
        Schema schema = SchemaLoader.load(raw)
        def found = []
        try {
            schema.validate(new JSONObject(new JsonBuilder(params).toString()))
        } catch (ValidationException e) {
            collect(e.toJSON(), found)
        }
        return found
    }

    static void collect(json, List found) {
        def causes = json['causingExceptions']
        if (causes.length() == 0) {
            found << "${json['pointerToViolation']}: ${json['message']}"
        }
        for (cause in causes) {
            collect(cause, found)
        }
    }
}
"#,
    );
    write(
        root.path(),
        "main.nf",
        r##"def schema = '{"$schema": "http://json-schema.org/draft-07/schema", "definitions": {"g": {"properties": {"n": {"type": "integer"}, "skip": {"type": "integer"}, "mode": {"type": "string", "enum": ["a", "b"]}}, "required": ["input"]}}, "allOf": [{"$ref": "#/definitions/g"}]}'
workflow {
    println(Check.errors(schema, [input: 'x', n: 1, skip: 'ignored']))
    println(Check.errors(schema, [n: 'x', mode: 'c']).sort().join('\n'))
}
"##,
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    let lines: Vec<&str> = output.0.lines().collect();
    assert_eq!(lines[0], "[]");
    let errors = &lines[1..];
    assert_eq!(
        errors,
        [
            "#/mode: c is not a valid enum value",
            "#/n: expected type: Integer, found: String",
            "#: required key [input] not found",
        ]
    );
}

/// Legacy processes may set variables among their sections, which later sections read, and
/// an assignment among the directives sets a variable, not the directive. Checked with
/// Nextflow 26.04.6 and `NXF_SYNTAX_PARSER=v1`.
#[test]
fn process_sections_set_variables() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "process P {\n    errorStrategy = 'ignore'\n    echo true\n\n    input:\n    val x\n\n    \
         output:\n    path 'out.txt', emit: out\n\n    def y = 7\n    script:\n    \
         \"echo $x $y $errorStrategy\"\n}\nworkflow {\n    P(channel.of(3))\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let options = nextflow_runtime::Options::default();
    let (_, requests) =
        nextflow_runtime::Run::start(&module, &options, &mut Output::default()).expect("starts");
    assert_eq!(requests[0].script, "#!/bin/bash -ue\necho 3 7 ignore\n");
    let error_strategy = requests[0]
        .directives
        .iter()
        .find(|directive| &*directive.name == "errorStrategy");
    assert!(error_strategy.is_none(), "{error_strategy:?}");
}

/// Legacy includes may name the project directory in the source and break the line
/// before `as`.
#[test]
fn includes_read_the_project_directory() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(root.path().join("modules")).expect("creates modules");
    write(
        root.path(),
        "modules/show.nf",
        "workflow SHOW {\n    main:\n    println('shown')\n}\n",
    );
    write(
        root.path(),
        "main.nf",
        "include { SHOW\n    as SHOW_IT } from \"${projectDir}/modules/show\"\nworkflow {\n    SHOW_IT()\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "shown\n");
}
