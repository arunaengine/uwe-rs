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
fn executes_module_statements() {
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

/// An include inside an `if` takes effect when its branch runs, as with Nextflow 26.04.6 and
/// the legacy parser, which prints `B` for this script.
#[test]
fn evaluates_conditional_includes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "def platform = 'b'
if (platform == 'a') {
    include { A as C } from './a'
} else {
    include { B as C } from './a'
}
workflow {
    C()
}
",
    );
    write(
        root.path(),
        "a.nf",
        "workflow A { println('A') }\nworkflow B { println('B') }\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "B\n");
}

/// `params.x = value` among the statements of a script runs in order with them, and only
/// sets a param that nothing set before, as with Nextflow 26.04.6 and the legacy parser,
/// which prints `b 1` for this script.
#[test]
fn orders_parameter_assignments() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "if (true) {
    platform = 'b'
}
params.platform = platform
params.count = 1
params.count = 2
def show() { \"${params.platform} ${params.count}\" }
workflow {
    println(show())
}
",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "b 1\n");
}

/// A `lib/` class like the `NfcoreSchema` of older nf-core pipelines, which reads a schema
/// with `org.json`, drops an ignored param from it and validates params with everit. The
/// violations are those of everit 1.14.1, which
/// `nextflow-evaluator/tests/fixtures/oracle/org_json/schema.groovy` prints.
#[test]
fn validates_library_classes() {
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
fn binds_process_sections() {
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
fn resolves_project_includes() {
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

/// A legacy include source may add strings to a script variable that holds the project
/// directory, as in ProGFASTAGen, which Nextflow 26.04.6 runs.
#[test]
fn resolves_directory_expressions() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "show.nf",
        "workflow SHOW {\n    main:\n    println('shown')\n}\n",
    );
    // The second one, as in MoP, gives the file to a GString; Nextflow 26.04.6 runs both.
    for head in [
        "PROJECT_DIR = workflow.projectDir\ninclude { SHOW } from PROJECT_DIR + '/show.nf'",
        "def m = file(\"$baseDir/show.nf\")\ninclude { SHOW } from \"${m}\"",
    ] {
        write(
            root.path(),
            "main.nf",
            &format!("{head}\nworkflow {{\n    SHOW()\n}}\n"),
        );
        let module = Loader::default()
            .load(&root.path().join("main.nf"))
            .expect("loads");
        let mut output = Output::default();
        nextflow_runtime::run(&module, &mut output).expect("runs");
        assert_eq!(output.0, "shown\n");
    }
}

/// An include inside a workflow, as in bactmap 1.0.0, links and runs the included script
/// when the workflow runs, with the params it adds; Nextflow 21.04.0 prints `show loaded 5`
/// and `show 5`, and without `addParams` `show loaded 1` and `show 1`.
#[test]
fn includes_inside_workflows() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "show.nf",
        "params.x = 1\nprintln \"show loaded ${params.x}\"\nworkflow SHOW {\n    main:\n    println(\"show ${params.x}\")\n}\n",
    );
    for (params, printed) in [
        (" addParams(x: 5)", "show loaded 5\nshow 5\n"),
        ("", "show loaded 1\nshow 1\n"),
    ] {
        write(
            root.path(),
            "main.nf",
            &format!("workflow {{\n    include {{ SHOW }} from './show'{params}\n    SHOW()\n}}\n"),
        );
        let module = Loader::default()
            .load(&root.path().join("main.nf"))
            .expect("loads");
        let mut output = Output::default();
        nextflow_runtime::run(&module, &mut output).expect("runs");
        assert_eq!(output.0, printed);
    }
}

/// An include source with an extension other than `.nf`, like `lib/utils.groovy` in the
/// MGnify pipelines, names that file, and a directory with a slash at the end, as in pacvar,
/// its `main.nf`, as Nextflow's `realModulePath` finds them.
#[test]
fn includes_groovy_files() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(root.path().join("lib")).expect("creates lib");
    std::fs::create_dir(root.path().join("sub")).expect("creates sub");
    write(
        root.path(),
        "lib/utils.groovy",
        "def hello() {\n    'hello'\n}\n",
    );
    write(
        root.path(),
        "sub/main.nf",
        "def there() {\n    'there'\n}\n",
    );
    write(
        root.path(),
        "main.nf",
        "include { hello } from './lib/utils.groovy'\ninclude { there } from './sub/'\n\
         workflow {\n    println(hello() + ' ' + there())\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "hello there\n");
}

/// `evaluate(file)` runs a Groovy script, like the `Map.metaClass` addition of the Swedish
/// Earth Biogenome Project's lib/MapExtended.groovy, which reaches no maps, and `deepMerge` is
/// Nextflow's own; Nextflow 26.04.6 prints `[x:1, n:[p:1, q:3, r:4], z:null, y:2]`.
#[test]
fn evaluates_groovy_scripts() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "extend.groovy",
        "Map.metaClass.deepMerge = { Map rhs ->\n    def lhs = delegate\n    lhs + rhs\n}\n",
    );
    let extend = root.path().join("extend.groovy");
    write(
        root.path(),
        "main.nf",
        &format!(
            "evaluate(new File('{}'))\nworkflow {{\n    def a = [x: 1, n: [p: 1, q: 2], z: 5]\n    \
             println(a.deepMerge([n: [q: 3, r: 4], y: 2, z: null]))\n}}\n",
            extend.display()
        ),
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "[x:1, n:[p:1, q:3, r:4], z:null, y:2]\n");
}

/// A class that a script defines, which closures of the script use, as
/// multiplesequencealign's `Utils` in a subworkflow script.
#[test]
fn binds_script_classes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "main.nf",
        "workflow W {\n    main:\n    channel.of([[a: 1]]).map { meta -> Utils.clean(meta[0].a) }.view()\n}\n\
         workflow {\n    W()\n}\n\nclass Utils {\n    public static clean(x) {\n        return \"clean ${x}\"\n    }\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "clean 1\n");
}

/// An include with `addParams(...)` loads the script again with params of its own: those of
/// the including script with the given ones added, as Nextflow 26.04.6 prints
/// `m loaded 5`, `m loaded 6`, `5 hi`, `6 hi` and `null` for these scripts.
#[test]
fn reloads_parameterized_includes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    write(
        root.path(),
        "m.nf",
        "params.options = [:]\nparams.greeting = 'hello'\nprintln \"m loaded ${params.options.x}\"\n\
         def show() { \"${params.options.x} ${params.greeting}\" }\n",
    );
    write(
        root.path(),
        "main.nf",
        "params.greeting = 'hi'\ndef opts = [x: 5]\n\
         include { show as A } from './m' addParams( options: opts )\n\
         include { show as B } from './m' addParams( options: [x: 6] )\n\
         workflow {\n    println(A())\n    println(B())\n    println(params.options)\n}\n",
    );
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .expect("loads");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "m loaded 5\nm loaded 6\n5 hi\n6 hi\nnull\n");
}
