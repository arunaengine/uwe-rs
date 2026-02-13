//! Tests how scripts parse into declarations and sections, and which script errors are reported.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    Agent, BodyKind, ExpressionKind, Function, Item, Literal, Name, NamedOutput, OutputTarget,
    ParameterDeclaration, Process, ProcessInput, TypedName, Workflow, WorkflowTake,
};
use nextflow_parser::parse_script;

const MISSING_LABEL: &str =
    "The `script:` or `exec:` label is required when other sections are present";
const INVALID_PROCESS: &str =
    "Invalid process definition -- check for missing or out-of-order section labels";
const INVALID_WORKFLOW: &str =
    "Invalid workflow definition -- check for missing or out-of-order section labels";

/// Parses a script that must have no errors.
fn parse(source: &str) -> Vec<Item> {
    let parsed = parse_script(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected errors: {:?}",
        parsed.errors
    );
    parsed.tree.items
}

fn messages(source: &str) -> Vec<String> {
    let parsed = parse_script(source);
    parsed
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect()
}

fn texts(names: &[Name]) -> Vec<&str> {
    names.iter().map(|name| name.text.as_str()).collect()
}

fn typed_names(names: &[TypedName]) -> Vec<&str> {
    names.iter().map(|typed| typed.name.text.as_str()).collect()
}

fn process(item: &Item) -> &Process {
    match item {
        Item::Process(process) => process,
        other => panic!("expected a process, found {other:?}"),
    }
}

fn workflow(item: &Item) -> &Workflow {
    match item {
        Item::Workflow(workflow) => workflow,
        other => panic!("expected a workflow, found {other:?}"),
    }
}

/// The name of a `name` or `name = value` output.
fn output_name(output: &NamedOutput) -> &str {
    match output {
        NamedOutput::Named { target, .. } => &target.name.text,
        other => panic!("expected a named output, found {other:?}"),
    }
}

#[test]
fn feature_flags() {
    let items = parse("nextflow.enable.dsl = 2\nnextflow.preview.topic = true\n");
    assert_eq!(items.len(), 2);
    let Item::FeatureFlag { name, value, .. } = &items[0] else {
        panic!("expected a feature flag, found {:?}", items[0]);
    };
    assert_eq!(texts(name), ["enable", "dsl"]);
    let two = ExpressionKind::Literal(Literal::Integer("2".to_owned()));
    assert_eq!(value.kind, two);
}

#[test]
fn rejects_computed_flag() {
    assert_eq!(
        messages("nextflow.enable.types = !false\n"),
        ["Feature flag value must be a literal value (number, string, true/false)"]
    );
}

#[test]
fn include_names() {
    let items = parse(
        "include { FASTQC                 } from '../modules/nf-core/fastqc/main'
include { paramsSummaryMap       } from 'plugin/nf-schema'
include { SAMTOOLS_SORT as SORT; SAMTOOLS_INDEX } from './modules/samtools'
include {
    softwareVersionsToYAML
    methodsDescriptionText
} from '../subworkflows/nf-core/utils_nfcore_pipeline'
",
    );
    let includes: Vec<_> = items
        .iter()
        .map(|item| match item {
            Item::Include(include) => include,
            other => panic!("expected an include, found {other:?}"),
        })
        .collect();
    assert_eq!(includes.len(), 4);
    assert_eq!(includes[0].source.text, "../modules/nf-core/fastqc/main");
    assert_eq!(includes[1].source.text, "plugin/nf-schema");
    let sorted = &includes[2].names;
    assert_eq!(sorted[0].name.text, "SAMTOOLS_SORT");
    assert_eq!(
        sorted[0].alias.as_ref().map(|alias| &*alias.text),
        Some("SORT")
    );
    assert_eq!(sorted[1].name.text, "SAMTOOLS_INDEX");
    assert_eq!(sorted[1].alias, None);
    assert_eq!(includes[3].names.len(), 2);
}

#[test]
fn rejects_import() {
    let parsed = parse_script("import groovy.json.JsonSlurper\n");
    assert!(matches!(parsed.tree.items[..], [Item::Import { .. }]));
    let message =
        "Groovy `import` declarations are not supported -- use fully-qualified name inline instead";
    assert_eq!(parsed.errors[0].message, message);
}

#[test]
fn parameter_block() {
    let items = parse(
        "params {
    // Path to the samplesheet
    input: Path
    outdir: String = 'results'
    save_merged = false
}

workflow {
    main:
    println(params.outdir)
}
",
    );
    let Item::Parameters { parameters, .. } = &items[0] else {
        panic!("expected a params block, found {:?}", items[0]);
    };
    let shapes: Vec<_> = parameters
        .iter()
        .map(|declaration| match declaration {
            ParameterDeclaration::Parameter {
                name,
                annotation,
                default,
            } => (&*name.text, annotation.is_some(), default.is_some()),
            other => panic!("expected a parameter, found {other:?}"),
        })
        .collect();
    let expected = [
        ("input", true, false),
        ("outdir", true, true),
        ("save_merged", false, true),
    ];
    assert_eq!(shapes, expected);
}

#[test]
fn rejects_parameter_statement() {
    let source = "params {\n    input.path = 'x'\n}\n\nworkflow {\n}\n";
    assert_eq!(messages(source), ["Invalid parameter declaration"]);
}

#[test]
fn legacy_parameters() {
    let items = parse("params.outdir = 'results'\nparams.multiqc.title = null\n");
    let paths: Vec<_> = items
        .iter()
        .map(|item| match item {
            Item::LegacyParameter { path, .. } => texts(path),
            other => panic!("expected a legacy parameter, found {other:?}"),
        })
        .collect();
    assert_eq!(paths, [vec!["outdir"], vec!["multiqc", "title"]]);
}

#[test]
fn rejects_mixed_parameters() {
    let source = "params {\n    a = 1\n}\nparams.b = 2\n\nworkflow {\n}\n";
    assert_eq!(
        messages(source),
        ["Legacy parameter declarations cannot be mixed with the params block"]
    );
}

#[test]
fn record_fields() {
    let items = parse(
        "record Sample {
    id: String
    fastq_1: Path
    fastq_2: Path?
}
",
    );
    let Item::Record { name, fields, .. } = &items[0] else {
        panic!("expected a record, found {:?}", items[0]);
    };
    assert_eq!(name.text, "Sample");
    assert_eq!(typed_names(fields), ["id", "fastq_1", "fastq_2"]);
    assert!(fields.iter().all(|field| field.annotation.is_some()));
}

#[test]
fn rejects_incomplete_records() {
    assert_eq!(messages("record Sample {}\n"), ["Missing record body"]);
    assert_eq!(
        messages("record Sample {\n    id\n}\n"),
        ["Missing field type"]
    );
}

#[test]
fn enum_variants() {
    let items = parse("enum Strandedness {\n    FORWARD,\n    REVERSE,\n    UNSTRANDED,\n}\n");
    let Item::Enum { name, variants, .. } = &items[0] else {
        panic!("expected an enum, found {:?}", items[0]);
    };
    assert_eq!(name.text, "Strandedness");
    assert_eq!(texts(variants), ["FORWARD", "REVERSE", "UNSTRANDED"]);
}

#[test]
fn process_sections() {
    let items = parse(
        r#"process FASTQC {
    tag "${meta.id}"
    label 'process_medium'

    conda "${moduleDir}/environment.yml"
    container "${ workflow.containerEngine == 'singularity' ? 'https://depot.galaxyproject.org/singularity/fastqc:0.12.1--hdfd78af_0' : 'biocontainers/fastqc:0.12.1--hdfd78af_0' }"

    input:
    tuple val(meta), path(reads)

    output:
    tuple val(meta), path("*.html"), emit: html
    tuple val(meta), path("*.zip") , emit: zip
    path  "versions.yml"           , emit: versions

    when:
    task.ext.when == null || task.ext.when

    script:
    def args = task.ext.args ?: ''
    def prefix = task.ext.prefix ?: "${meta.id}"
    """
    fastqc ${args} --threads ${task.cpus} ${reads}
    """

    stub:
    def prefix = task.ext.prefix ?: "${meta.id}"
    """
    touch ${prefix}.html
    """
}
"#,
    );
    let fastqc = process(&items[0]);
    assert_eq!(fastqc.name.text, "FASTQC");
    assert_eq!(fastqc.directives.len(), 4);
    assert!(matches!(fastqc.inputs[..], [ProcessInput::Statement(_)]));
    assert_eq!(fastqc.outputs.len(), 3);
    let statements = |output| matches!(output, &NamedOutput::Statement(_));
    assert!(fastqc.outputs.iter().all(statements));
    assert!(fastqc.when.is_some());
    let body = fastqc.body.as_ref().expect("a script body");
    assert_eq!(body.kind, BodyKind::Script);
    assert_eq!(body.block.statements.len(), 3);
    assert_eq!(
        fastqc.stub.as_ref().map(|stub| stub.statements.len()),
        Some(2)
    );
}

#[test]
fn implicit_script_body() {
    let items = parse("process SAY_HELLO {\n    \"\"\"\n    echo 'Hello world!'\n    \"\"\"\n}\n");
    let hello = process(&items[0]);
    assert!(hello.directives.is_empty());
    let body = hello.body.as_ref().expect("an implicit body");
    assert_eq!(body.kind, BodyKind::Implicit);
    assert_eq!(body.block.statements.len(), 1);
}

#[test]
fn implicit_after_when() {
    let items = parse("process CHECK {\n    when:\n    params.run\n\n    \"echo run\"\n}\n");
    let check = process(&items[0]);
    assert!(check.when.is_some());
    let kind = check.body.as_ref().map(|body| body.kind);
    assert_eq!(kind, Some(BodyKind::Implicit));
}

#[test]
fn requires_script_label() {
    let inputs = "process HELLO {
    input:
    val greeting
    val name

    \"\"\"
    echo '${greeting} ${name}!'
    \"\"\"
}
";
    assert_eq!(messages(inputs), [MISSING_LABEL]);
    let directives = "process HELLO {\n    cpus 2\n\n    \"echo hello\"\n}\n";
    assert_eq!(messages(directives), [MISSING_LABEL]);
}

#[test]
fn missing_process_body() {
    assert_eq!(
        messages("process EMPTY {\n}\n"),
        ["Missing process script body"]
    );
}

#[test]
fn rejects_section_order() {
    let reordered = "process HELLO {
    output:
    val x

    input:
    val y

    script:
    \"echo\"
}
";
    assert_eq!(messages(reordered), [INVALID_PROCESS]);
    let repeated = "process HELLO {\n    input:\n    val x\n\n    input:\n    val y\n\n    script:\n    \"echo\"\n}\n";
    assert_eq!(messages(repeated), [INVALID_PROCESS]);
    let unlabeled = "process HELLO {\n    input:\n    val x\n}\n";
    assert_eq!(messages(unlabeled), [INVALID_PROCESS]);
}

#[test]
fn labels_need_separator() {
    let source = "process ONE_LINE { script: \"echo\" }\n";
    assert_eq!(messages(source), [INVALID_PROCESS]);
}

#[test]
fn outputs_after_stub() {
    let items = parse(
        "process MAHESH {
    input:
    val x

    exec:
    y = x

    stub:
    y = 0

    output:
    val y
}
",
    );
    let mahesh = process(&items[0]);
    let kind = mahesh.body.as_ref().map(|body| body.kind);
    assert_eq!(kind, Some(BodyKind::Exec));
    assert!(mahesh.stub.is_some());
    assert_eq!(mahesh.outputs.len(), 1);
}

#[test]
fn typed_process_inputs() {
    let items = parse(
        "nextflow.enable.types = true

process ALIGN {
    input:
    meta: Map
    reads: Path
    tuple(id: String, fastq: Path)
    record(
        id: String,
        bam: Path,
    )

    output:
    bam: Path = file('*.bam')

    topic:
    stdout() >> 'versions'

    script:
    \"\"\"
    align ${reads}
    \"\"\"
}
",
    );
    let align = process(&items[1]);
    let [
        ProcessInput::Named(meta),
        ProcessInput::Named(reads),
        ProcessInput::Tuple(tuple),
        ProcessInput::Record(record),
    ] = &align.inputs[..]
    else {
        panic!("unexpected inputs: {:?}", align.inputs);
    };
    assert_eq!([&*meta.name.text, &*reads.name.text], ["meta", "reads"]);
    assert!(meta.annotation.is_some());
    assert_eq!(typed_names(tuple), ["id", "fastq"]);
    assert_eq!(typed_names(record), ["id", "bam"]);
    let [NamedOutput::Named { target, value }] = &align.outputs[..] else {
        panic!("unexpected outputs: {:?}", align.outputs);
    };
    assert_eq!(target.name.text, "bam");
    assert!(target.annotation.is_some() && value.is_some());
    assert_eq!(align.topics.len(), 1);
}

#[test]
fn rejects_single_tuple() {
    let source = "process HELLO {\n    input:\n    tuple(id: String)\n\n    script:\n    \"\"\n}\n";
    assert_eq!(
        messages(source),
        ["Process tuple input must have more than one component"]
    );
}

#[test]
fn rejects_invalid_directive() {
    let source = "process HELLO {\n    container = 'ubuntu'\n\n    script:\n    \"\"\n}\n";
    assert_eq!(messages(source), ["Invalid process directive"]);
    let topic =
        "process HELLO {\n    topic:\n    versions = stdout()\n\n    script:\n    \"\"\n}\n";
    assert_eq!(messages(topic), ["Invalid process topic statement"]);
}

#[test]
fn named_workflow_sections() {
    let items = parse(
        "workflow NFCORE_BAMTOFASTQ {

    take:
    samplesheet // channel: samplesheet read in from --input
    fasta: Path

    main:
    BAMTOFASTQ(samplesheet, params.multiqc_config)

    emit:
    multiqc_report = BAMTOFASTQ.out.multiqc_report // channel: /path/to/multiqc_report.html
    versions
}
",
    );
    let flow = workflow(&items[0]);
    assert_eq!(
        flow.name.as_ref().map(|name| &*name.text),
        Some("NFCORE_BAMTOFASTQ")
    );
    let takes: Vec<_> = flow
        .takes
        .iter()
        .map(|take| match take {
            WorkflowTake::Named(typed) => &*typed.name.text,
            other => panic!("expected a named take, found {other:?}"),
        })
        .collect();
    assert_eq!(takes, ["samplesheet", "fasta"]);
    assert_eq!(flow.main.statements.len(), 1);
    let emits: Vec<_> = flow.emits.iter().map(output_name).collect();
    assert_eq!(emits, ["multiqc_report", "versions"]);
}

#[test]
fn entry_workflow_handlers() {
    let items = parse(
        "workflow {
    main:
    ch_samples = channel.of(1, 2, 3)

    publish:
    samples = ch_samples

    onComplete:
    log.info('done')

    onError:
    log.error('failed')
}

output {
    samples {
        path 'samples'
    }
}
",
    );
    let entry = workflow(&items[0]);
    assert_eq!(entry.name, None);
    let publishers: Vec<_> = entry.publishers.iter().map(output_name).collect();
    assert_eq!(publishers, ["samples"]);
    assert!(entry.on_complete.is_some() && entry.on_error.is_some());
    let Item::Output { targets, .. } = &items[1] else {
        panic!("expected an output block, found {:?}", items[1]);
    };
    let [OutputTarget::Target { name, body, .. }] = &targets[..] else {
        panic!("unexpected output targets: {targets:?}");
    };
    assert_eq!(name.text, "samples");
    assert_eq!(body.statements.len(), 1);
}

#[test]
fn implicit_workflow_main() {
    let items = parse("workflow {\n    SAY_HELLO()\n    SAY_BYE()\n}\n");
    let entry = workflow(&items[0]);
    assert_eq!(entry.main.statements.len(), 2);
    assert!(entry.takes.is_empty() && entry.emits.is_empty());
    let single = parse("workflow { SAY_HELLO() }\n");
    assert_eq!(workflow(&single[0]).main.statements.len(), 1);
}

#[test]
fn rejects_workflow_order() {
    let reordered = "workflow FLOW {\n    main:\n    FOO()\n\n    take:\n    x\n}\n";
    assert_eq!(messages(reordered), [INVALID_WORKFLOW]);
    let empty_main = "workflow FLOW {\n    take:\n    x\n\n    main:\n\n    emit:\n    y = x\n}\n";
    assert_eq!(messages(empty_main), [INVALID_WORKFLOW]);
    let one_line = "workflow FLOW { main: FOO() }\n";
    assert_eq!(messages(one_line), [INVALID_WORKFLOW]);
    let take_only = "workflow FLOW {\n    take:\n    x\n}\n";
    assert_eq!(messages(take_only), [INVALID_WORKFLOW]);
}

#[test]
fn rejects_misplaced_sections() {
    let entry = "workflow {\n    take:\n    x\n\n    main:\n    FOO(x)\n}\n";
    assert_eq!(
        messages(entry),
        ["Entry workflow cannot have a take section"]
    );
    let named =
        "workflow FLOW {\n    main:\n    FOO()\n\n    onComplete:\n    log.info('done')\n}\n";
    assert_eq!(
        messages(named),
        ["Named workflow cannot have an onComplete section"]
    );
}

#[test]
fn rejects_unnamed_emits() {
    let source = "workflow HELLO {\n    emit:\n    'hello'\n    target = 'world'\n}\n";
    assert_eq!(
        messages(source),
        ["Every emit must be assigned to a name when there are multiple emits"]
    );
    let assignment = "workflow HELLO {\n    emit:\n    target <<= 'world'\n}\n";
    assert_eq!(
        messages(assignment),
        ["Invalid workflow emit -- must be a name, assignment, or expression"]
    );
}

#[test]
fn function_definitions() {
    let items = parse(
        "def greet(name) {
    return \"Hello ${name}!\"
}

def add(a: Integer, b: Integer) -> Integer {
    a + b
}

String shout(String text) {
    text.toUpperCase()
}

def getGenomeAttribute(attribute) {
    if (params.genomes && params.genome && params.genomes.containsKey(params.genome)) {
        return params.genomes[params.genome][attribute]
    }
    return null
}
",
    );
    let functions: Vec<&Function> = items
        .iter()
        .map(|item| match item {
            Item::Function(function) => &**function,
            other => panic!("expected a function, found {other:?}"),
        })
        .collect();
    let names: Vec<_> = functions
        .iter()
        .map(|function| &*function.name.text)
        .collect();
    assert_eq!(names, ["greet", "add", "shout", "getGenomeAttribute"]);
    assert_eq!(functions[0].parameters.len(), 1);
    assert_eq!(functions[0].return_type, None);
    assert_eq!(functions[1].parameters.len(), 2);
    assert!(functions[1].return_type.is_some());
    assert!(functions[2].return_type.is_some());
    assert_eq!(functions[3].body.statements.len(), 2);
}

#[test]
fn agent_sections() {
    let items = parse(
        "agent eval_agent {
    model 'claude-sonnet'

    input:
    question: String

    output:
    answer: String

    prompt:
    \"\"\"
    Answer this question: ${question}
    \"\"\"
}
",
    );
    let Item::Agent(agent) = &items[0] else {
        panic!("expected an agent, found {:?}", items[0]);
    };
    let agent: &Agent = agent;
    assert_eq!(agent.name.text, "eval_agent");
    assert_eq!(agent.directives.len(), 1);
    assert!(matches!(agent.inputs[..], [ProcessInput::Named(_)]));
    assert_eq!(agent.outputs.len(), 1);
    assert_eq!(agent.prompt.statements.len(), 1);
    let missing = "agent broken {\n    input:\n    question: String\n}\n";
    assert_eq!(
        messages(missing),
        ["Invalid agent definition -- check for missing or out-of-order section labels"]
    );
}

#[test]
fn rejects_mixed_statements() {
    let source = "println 'Hello world!'\n\nworkflow {\n}\n";
    assert_eq!(
        messages(source),
        [
            "Statements cannot be mixed with script declarations -- move statements into a process, workflow, or function"
        ]
    );
    assert!(messages("println 'Hello world!'\nx = 1\n").is_empty());
}

#[test]
fn requires_entry_workflow() {
    assert_eq!(
        messages("params {\n    input: Path\n}\n"),
        ["Params block cannot be defined without an entry workflow"]
    );
    let twice = "workflow {\n}\n\nworkflow {\n}\n";
    assert_eq!(messages(twice), ["Entry workflow defined more than once"]);
}

#[test]
fn incomplete_declaration() {
    assert_eq!(messages("workflow.\n"), ["Incomplete declaration"]);
}

#[test]
fn recovers_after_error() {
    let parsed = parse_script(
        "include { } from './modules/broken'

process GOOD {
    script:
    \"echo good\"
}

enum Broken { A B }

workflow {
    GOOD()
}
",
    );
    assert_eq!(parsed.errors.len(), 2, "errors: {:?}", parsed.errors);
    let kinds: Vec<_> = parsed
        .tree
        .items
        .iter()
        .map(|item| match item {
            Item::Process(_) => "process",
            Item::Workflow(_) => "workflow",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["process", "workflow"]);
}
