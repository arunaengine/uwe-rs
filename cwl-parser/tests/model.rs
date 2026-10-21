//! Tests the typed model: tools, workflows, types, requirements, expressions and field checks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use cwl_parser::model::{
    Argument, ArraySchema, Class, Dynamic, ExpressionKind, LinkMerge, PickValue, Process,
    RequirementClass, ScatterMethod, Type,
};
use cwl_parser::source::Memory;
use cwl_parser::{Error, Loader, Value};

const URL: &str = "file:///work/doc.cwl";

fn outcome(text: &str, extra: &[(&str, &str)]) -> Result<Arc<Process>, Error> {
    let mut memory = Memory::default();
    memory.insert(URL, text);
    for (url, text) in extra {
        memory.insert(url, text);
    }
    Loader::new(memory)
        .load(URL)
        .map(|document| document.process)
}

fn load(text: &str) -> Arc<Process> {
    outcome(text, &[]).unwrap_or_else(|error| panic!("{error}"))
}

fn failure(text: &str) -> Error {
    match outcome(text, &[]) {
        Ok(process) => panic!("loaded {}", process.id),
        Err(error) => error,
    }
}

const TOOL: &str = "cwlVersion: v1.2\nclass: CommandLineTool\n\
    $namespaces: {edam: 'http://edamontology.org/'}\n\
    requirements:\n  DockerRequirement: {dockerPull: alpine}\n  \
    ResourceRequirement: {coresMin: 2, ramMin: $(inputs.size * 2)}\n\
    hints:\n  - {class: 'sbg:AWSInstanceType', value: c5.xlarge}\n\
    baseCommand: [sort, -k]\n\
    arguments: [-n, {prefix: -o, valueFrom: $(runtime.outdir)/out.txt}]\n\
    inputs:\n  reads:\n    type: File[]?\n    format: edam:format_1930\n    \
    secondaryFiles: [.bai?]\n    inputBinding: {position: 1}\n  \
    mode:\n    type: {type: enum, symbols: [fast, slow]}\n    default: fast\n  size: int\n\
    outputs:\n  sorted:\n    type: File\n    outputBinding: {glob: out.txt}\n\
    stdout: log.txt\n";

#[test]
fn reads_command_tools() {
    let tool = load(TOOL);
    let Class::CommandLineTool(command) = &tool.class else {
        panic!("a command line tool");
    };
    assert_eq!(command.base_command, ["sort", "-k"]);
    let [Argument::Text(flag), Argument::Binding(output)] = command.arguments.as_slice() else {
        panic!("two arguments");
    };
    assert_eq!(flag.kind(), ExpressionKind::Literal);
    assert_eq!(output.prefix.as_deref(), Some("-o"));
    let value = output.value_from.as_ref().expect("valueFrom is set");
    assert_eq!(value.kind(), ExpressionKind::Interpolation);
    assert_eq!(
        command.stdout.as_ref().map(|stdout| stdout.text.as_str()),
        Some("log.txt")
    );
    let reads = &tool.inputs[1];
    assert_eq!(reads.id, "file:///work/doc.cwl#reads");
    let array = Type::Array(Box::new(ArraySchema {
        items: Type::File,
        name: None,
        label: None,
        doc: Vec::new(),
        binding: None,
    }));
    assert_eq!(reads.value_type, Type::Union(vec![Type::Null, array]));
    assert_eq!(reads.format[0].text, "http://edamontology.org/format_1930");
    assert_eq!(reads.secondary_files[0].pattern.text, ".bai");
    assert_eq!(
        reads.secondary_files[0].required,
        Some(Dynamic::Value(false))
    );
    let binding = reads.binding.as_ref().expect("an input binding");
    assert_eq!(binding.position, Some(Dynamic::Value(1)));
    let Type::Enum(mode) = &tool.inputs[0].value_type else {
        panic!("an enum");
    };
    assert_eq!(mode.symbols, ["fast", "slow"]);
    let default = tool.inputs[0].default.as_ref().map(|node| &node.value);
    assert_eq!(default, Some(&Value::String("fast".into())));
    let glob = &tool.outputs[0]
        .binding
        .as_ref()
        .expect("an output binding")
        .glob;
    assert_eq!(glob[0].text, "out.txt");
}

#[test]
fn types_known_requirements() {
    let tool = load(TOOL);
    let [docker, resource] = tool.requirements.as_slice() else {
        panic!("two requirements");
    };
    let RequirementClass::Docker(docker) = &docker.class else {
        panic!("a DockerRequirement");
    };
    assert_eq!(docker.docker_pull.as_deref(), Some("alpine"));
    let RequirementClass::Resource(resource) = &resource.class else {
        panic!("a ResourceRequirement");
    };
    assert_eq!(resource.cores_min, Some(Dynamic::Value(2.0)));
    let Some(Dynamic::Expression(ram)) = &resource.ram_min else {
        panic!("an expression");
    };
    assert_eq!(ram.kind(), ExpressionKind::Javascript);
}

#[test]
fn keeps_unknown_requirements() {
    let tool = load(TOOL);
    let RequirementClass::Other { class, value } = &tool.hints[0].class else {
        panic!("an unknown hint");
    };
    assert_eq!(class, "sbg:AWSInstanceType");
    assert_eq!(
        value.get("value").and_then(|node| node.as_str()),
        Some("c5.xlarge")
    );
    let arvados = "cwlVersion: v1.2\nclass: CommandLineTool\n\
                   $namespaces: {arv: 'http://arvados.org/cwl#'}\n\
                   requirements: [{class: 'arv:RuntimeConstraints', keep_cache: 2}]\n\
                   inputs: []\noutputs: []\n";
    let tool = load(arvados);
    let RequirementClass::Other { class, .. } = &tool.requirements[0].class else {
        panic!("an unknown requirement");
    };
    assert_eq!(class, "http://arvados.org/cwl#RuntimeConstraints");
}

#[test]
fn reads_workflow_steps() {
    let workflow = "cwlVersion: v1.2\nclass: Workflow\n\
        requirements: [{class: ScatterFeatureRequirement}, {class: InlineJavascriptRequirement}]\n\
        inputs: {names: 'string[]', flag: boolean}\noutputs:\n  \
        out: {type: 'string[]', outputSource: echo/out, pickValue: all_non_null}\n\
        steps:\n  echo:\n    run:\n      class: ExpressionTool\n      \
        inputs: {name: string, other: string}\n      outputs: {out: string}\n      \
        expression: '${return {out: inputs.name};}'\n    \
        in:\n      name: names\n      other: {source: [names], linkMerge: merge_flattened, valueFrom: '$(self[0])'}\n    \
        out: [out]\n    scatter: [name, other]\n    scatterMethod: dotproduct\n    when: $(inputs.flag)\n";
    let workflow = load(workflow);
    let Class::Workflow(body) = &workflow.class else {
        panic!("a workflow");
    };
    let step = &body.steps[0];
    assert_eq!(step.scatter_method, Some(ScatterMethod::Dotproduct));
    assert_eq!(
        step.scatter,
        [
            "file:///work/doc.cwl#echo/name",
            "file:///work/doc.cwl#echo/other"
        ]
    );
    assert_eq!(
        step.when.as_ref().map(|when| when.kind()),
        Some(ExpressionKind::Reference)
    );
    let other = &step.inputs[1];
    assert_eq!(other.source, ["file:///work/doc.cwl#names"]);
    assert_eq!(other.link_merge, Some(LinkMerge::MergeFlattened));
    assert_eq!(workflow.outputs[0].pick_value, Some(PickValue::AllNonNull));
    assert_eq!(step.run.reference, "file:///work/doc.cwl#echo/run");
}

#[test]
fn resolves_named_types() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\n\
                requirements:\n  - class: SchemaDefRequirement\n    types:\n      - $import: types.yml\n\
                inputs:\n  sample: types.yml#Sample\noutputs: []\n";
    let types = "- {name: Sample, type: record, fields: {path: File}}\n";
    let tool = outcome(tool, &[("file:///work/types.yml", types)]);
    let tool = tool.unwrap_or_else(|error| panic!("{error}"));
    let expected = Type::Named("file:///work/types.yml#Sample".into());
    assert_eq!(tool.inputs[0].value_type, expected);
    let RequirementClass::SchemaDef { types } = &tool.requirements[0].class else {
        panic!("a SchemaDefRequirement");
    };
    let Type::Record(record) = &types[0] else {
        panic!("a record");
    };
    assert_eq!(record.fields[0].name, "path");
}

#[test]
fn rejects_unknown_fields() {
    let error = failure(
        "cwlVersion: v1.2\nclass: CommandLineTool\ninputs:\n  x: {type: File, colour: red}\noutputs: []\n",
    );
    assert_eq!(error.message, "the input parameter has no field `colour`");
    assert_eq!((error.location.line, error.location.column), (4, 19));
    let error =
        failure("cwlVersion: v1.2\nclass: CommandLineTool\ninputs: []\noutputs: [report.json]\n");
    assert_eq!(
        error.message,
        "an output parameter must be a map, not a string"
    );
}

#[test]
fn rejects_newer_features() {
    let workflow = "cwlVersion: v1.0\nclass: Workflow\ninputs: []\noutputs: []\n\
                    steps:\n  s:\n    run: {class: Operation, inputs: [], outputs: []}\n    \
                    in: []\n    out: []\n    when: $(true)\n";
    let error = failure(workflow);
    assert_eq!(
        error.message,
        "the Operation class needs cwlVersion v1.2 or later, not v1.0"
    );
    let error = failure(&workflow.replace(
        "class: Operation",
        "class: ExpressionTool, expression: '$({})'",
    ));
    assert_eq!(
        error.message,
        "`when` needs cwlVersion v1.2 or later, not v1.0"
    );
    // cwltool checks v1.0 documents against the v1.0 schema, where secondary files are
    // patterns and expressions only.
    let tool = "cwlVersion: v1.0\nclass: CommandLineTool\nbaseCommand: cat\noutputs: []\n\
                inputs:\n  reads: {type: File, secondaryFiles: [{pattern: .bai, required: false}]}\n";
    assert_eq!(
        failure(tool).message,
        "a secondary file with `pattern` needs cwlVersion v1.1 or later, not v1.0"
    );
}

#[test]
fn checks_expression_fields() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\ninputs: []\noutputs:\n  \
                n: {type: int, outputBinding: {outputEval: 'plain text'}}\n";
    let error = failure(tool);
    assert_eq!(
        error.message,
        "`plain text` is not an expression in $() or ${}"
    );
    let old = load(&tool.replace("v1.2", "v1.0"));
    let eval = old.outputs[0]
        .binding
        .as_ref()
        .and_then(|binding| binding.output_eval.as_ref());
    assert_eq!(eval.map(|eval| eval.kind()), Some(ExpressionKind::Literal));
}

#[test]
fn keeps_extension_fields() {
    let tool = load(
        "cwlVersion: v1.2\nclass: CommandLineTool\n$namespaces: {s: 'https://schema.org/'}\n\
                     s:author: someone\ninputs: []\noutputs: []\n",
    );
    assert_eq!(tool.extensions[0].key, "https://schema.org/author");
    assert_eq!(tool.extensions[0].value.as_str(), Some("someone"));
}
