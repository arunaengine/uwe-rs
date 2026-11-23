//! Lowers a composed workflow, a checked graph of tool nodes, into a wir document: a native op
//! per node in the language of its tool, and a map where a node maps over a list.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json, json};
use tool_definition::{Cardinality, Definition, Fit, PortKind, Report, SourceKind, ValueType};
use wir::{
    ArrayType, Document, Edge, Endpoint, Field, FileType, Graph, Kind, Language, Native, Op,
    Origin, Port, Source, Type,
};

use super::composed::ComposedWorkflow;
use super::library::ToolLibrary;

/// The EDAM namespace, for the formats that the schema of the inputs names.
const EDAM: &str = "http://edamontology.org/";

/// A lowered workflow: its wir document, the files it needs that lowering made, and the JSON
/// Schema keywords of the ports of its entry graph.
#[derive(Clone, Debug)]
pub struct Lowered {
    pub document: Document,
    /// Files the document names, like the entry scripts of Nextflow processes, by path.
    pub files: Vec<(PathBuf, String)>,
    /// Descriptions and EDAM formats of the inputs and outputs of the entry graph, by port.
    pub annotations: BTreeMap<String, Map<String, Json>>,
}

/// Why a composed workflow does not lower.
#[derive(Clone, Debug, PartialEq)]
pub enum LoweringError {
    /// The graph does not fit; the report holds the findings of its nodes and edges.
    Misfit(Box<Report>),
    /// A node cannot run, like the node of a tool without a source.
    Node { node: String, reason: String },
    /// The lowered document does not verify, which is a fault of the lowering.
    Invalid(Vec<String>),
}

impl fmt::Display for LoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoweringError::Misfit(report) => {
                let mut reasons = Vec::new();
                for node in report
                    .nodes
                    .iter()
                    .filter(|node| node.fit == Fit::DoesNotFit)
                {
                    for finding in node.findings.iter().filter(|f| f.fit == Fit::DoesNotFit) {
                        reasons.push(format!("node {}: {}", node.id, finding.reason));
                    }
                }
                for edge in report
                    .edges
                    .iter()
                    .filter(|edge| edge.fit == Fit::DoesNotFit)
                {
                    for finding in edge.findings.iter().filter(|f| f.fit == Fit::DoesNotFit) {
                        reasons.push(format!("edge: {}", finding.reason));
                    }
                }
                write!(formatter, "the graph does not fit: {}", reasons.join("; "))
            }
            LoweringError::Node { node, reason } => write!(formatter, "node {node}: {reason}"),
            LoweringError::Invalid(problems) => {
                write!(
                    formatter,
                    "the lowered document does not verify: {}",
                    problems.join("; ")
                )
            }
        }
    }
}

/// The outputs of a lowered node as its readers see them: arrays when the node maps.
struct Planned {
    path: String,
    outputs: Vec<Port>,
}

/// Lowers `workflow` with the tools of `library`. Source locations are relative to `base`,
/// the folder the document is meant for, when they lie below it; files that lowering makes,
/// like the entry scripts of Nextflow processes, go below `generated`.
pub fn lower(
    workflow: &ComposedWorkflow,
    library: &ToolLibrary,
    (base, generated): (&Path, &Path),
) -> Result<Lowered, LoweringError> {
    let tools = workflow.used_tools(library);
    let graph = workflow.graph_with(&tools);
    let report = tool_definition::graph::check(&graph, &tools);
    if report.fit == Fit::DoesNotFit {
        return Err(LoweringError::Misfit(Box::new(report)));
    }
    let mut sources: Vec<Source> = Vec::new();
    let mut files = Vec::new();
    let mut planned: Vec<Planned> = Vec::new();
    let mut ops = Vec::new();
    let mut graphs = Vec::new();
    let mut annotations = BTreeMap::new();
    let fed = fed_ports(workflow);
    for (node, verdict) in workflow.graph.nodes.iter().zip(&report.nodes) {
        let refuse = |reason: String| LoweringError::Node {
            node: node.id.clone(),
            reason,
        };
        let entry = (library.get(&node.tool, Some(&node.version))).ok_or_else(|| {
            refuse(format!(
                "tool {} version {} is not known",
                node.tool, node.version
            ))
        })?;
        let definition = &entry.definition;
        let path = name(&node.id);
        let source = (definition.source.as_ref())
            .ok_or_else(|| refuse(format!("the tool {} names no source to run", definition.id)))?;
        let located = located(&source.address, entry.origin.as_deref());
        let (language, entry_point) = match source.kind {
            SourceKind::Galaxy => (Language::Galaxy, None),
            SourceKind::Cwl => (Language::Cwl, None),
            SourceKind::Wdl => (Language::Wdl, Some(definition.id.clone())),
            SourceKind::Snakemake => (Language::Snakemake, Some(definition.id.clone())),
            SourceKind::NfCore | SourceKind::Nextflow => (Language::Nextflow, Some("node".into())),
            SourceKind::Container | SourceKind::Other => {
                let reason = format!("the tool {} has no source that uwe runs", definition.id);
                return Err(refuse(reason));
            }
        };
        let empty = BTreeSet::new();
        let ports = fed.get(node.id.as_str()).unwrap_or(&empty);
        let mut inputs = Vec::new();
        let mut over = Vec::new();
        // Inputs that nothing feeds stay out, so the block of the tool takes its defaults.
        for port in definition
            .inputs
            .iter()
            .filter(|port| ports.contains(port.name.as_str()))
        {
            if inputs
                .iter()
                .any(|known: &Port| known.name == name(&port.name))
            {
                continue;
            }
            let item = port_type(port.kind, port.cardinality, true);
            // The check decides per input, also for a list or a pair per sample.
            let r#type = match verdict.over.contains(&port.name) {
                true => {
                    over.push(name(&port.name));
                    array(item)
                }
                false => item,
            };
            inputs.push(Port {
                name: name(&port.name),
                r#type,
                flow: Default::default(),
                default: None,
            });
        }
        let values = match &node.parameters {
            Json::Object(values) => values.clone(),
            _ => Map::new(),
        };
        for (key, value) in &values {
            inputs.push(Port {
                name: name(key),
                r#type: json_type(value),
                flow: Default::default(),
                default: Some(value.clone()),
            });
        }
        // A workflow input may feed a parameter of the tool instead of a port.
        for input in &workflow.inputs {
            for end in input.to.iter().filter(|end| end.node == node.id) {
                let parameter = definition.input(&end.port).is_none();
                if parameter && !values.contains_key(&end.port) {
                    let shape = input.supply.shape.cardinality;
                    inputs.push(Port {
                        name: name(&end.port),
                        r#type: port_type(input.supply.kind, shape, true),
                        flow: Default::default(),
                        default: None,
                    });
                }
            }
        }
        let mut outputs: Vec<Port> = Vec::new();
        for output in &definition.outputs {
            let required = output.created_when.is_none() && !output.optional;
            let r#type = match (language, output.kind) {
                // Nextflow emits have no types; the ports that read them check their values.
                (Language::Nextflow, _) => Type::Dynamic,
                // A structured value keeps its shape, as no other native port may be dynamic.
                (_, PortKind::Value(ValueType::Object | ValueType::Array)) => {
                    let shaped = schema_type(&output.schema).ok_or_else(|| {
                        refuse(format!(
                            "wir cannot type the values of the output {}",
                            output.name
                        ))
                    })?;
                    match required {
                        true => shaped,
                        false => Type::Optional(Box::new(shaped)),
                    }
                }
                _ => port_type(output.kind, output.cardinality, required),
            };
            outputs.push(Port {
                name: name(&output.name),
                r#type,
                flow: Default::default(),
                default: None,
            });
        }
        let (location, digest) = match language {
            // A wrapper is named by its digest, so that workflows with a step of the same name
            // never replace each other's wrapper.
            Language::Nextflow => {
                let script = nextflow_entry(&located, definition, &inputs).map_err(refuse)?;
                let digest = task_model::Digest::sha256(script.as_bytes());
                let file = generated.join(format!("{path}-{}.nf", &digest.hex()[..12]));
                files.push((file.clone(), script));
                let location = file.to_string_lossy().into_owned();
                (location, Some(format!("sha256:{}", digest.hex())))
            }
            _ => {
                let digest =
                    Some(source.revision.clone()).filter(|text| text.starts_with("sha256:"));
                (relative(&located, base), digest)
            }
        };
        let id = match language {
            Language::Nextflow => name(&format!("{}-{}-{path}", definition.id, definition.version)),
            _ => name(&format!("{}-{}", definition.id, definition.version)),
        };
        let declared = Source {
            id,
            language,
            location,
            version: None,
            digest,
        };
        let id = declare(&mut sources, declared);
        let op = Op {
            path: path.clone(),
            kind: Kind::Native(Native {
                source: id.clone(),
                language,
                entry: entry_point,
                profiles: Vec::new(),
                configs: Vec::new(),
                targets: Vec::new(),
                config_values: None,
                until: Vec::new(),
                omit_from: Vec::new(),
                forcerun: Vec::new(),
                forceall: false,
            }),
            inputs: inputs.clone(),
            outputs: outputs.clone(),
            after: Vec::new(),
            depends: None,
            handles: Vec::new(),
            effects: Vec::new(),
            attributes: Default::default(),
            origin: Some(Origin {
                source: id,
                at: Some(definition.id.clone()),
            }),
            values: Default::default(),
        };
        let scatter = !over.is_empty();
        if scatter {
            let body = format!("{path}.round");
            let items: Vec<Port> = (inputs.iter())
                .map(|port| match over.contains(&port.name) {
                    true => Port {
                        r#type: item_of(&port.r#type),
                        ..port.clone()
                    },
                    false => port.clone(),
                })
                .collect();
            let inner = Op {
                inputs: items.clone(),
                ..op
            };
            graphs.push(body_graph(&body, inner, &items, &outputs));
            ops.push(map_op(&path, &body, &inputs, &outputs, &over));
        } else {
            ops.push(op);
        }
        let outputs = (outputs.into_iter())
            .map(|port| match scatter {
                true => Port {
                    r#type: array(port.r#type.clone()),
                    ..port
                },
                false => port,
            })
            .collect();
        planned.push(Planned { path, outputs });
    }
    let endpoint = |end: &tool_definition::End| Endpoint {
        op: Some(name(&end.node)),
        port: name(&end.port),
    };
    let graph_port = |port: &str| Endpoint {
        op: None,
        port: name(port),
    };
    let edge = |from: Endpoint, to: Endpoint| Edge {
        from,
        to,
        read: Default::default(),
    };
    let mut edges: Vec<Edge> = (workflow.graph.edges.iter())
        .map(|found| edge(endpoint(&found.from), endpoint(&found.to)))
        .collect();
    let mut entry_inputs = Vec::new();
    for input in &workflow.inputs {
        let shape = input.supply.shape.cardinality;
        let required = input.required && input.default.is_none();
        let mut r#type = port_type(input.supply.kind, shape, required);
        // A file without a format of its own takes the one format that the input declares.
        if let [format] = input.supply.formats.as_slice() {
            let iri = format!("{EDAM}{}", tool_definition::edam::term_id(format));
            set_format(&mut r#type, &iri);
        }
        entry_inputs.push(Port {
            name: name(&input.name),
            r#type,
            flow: Default::default(),
            default: input.default.clone(),
        });
        let mut keywords = Map::new();
        if let Some(text) = &input.description {
            keywords.insert("description".into(), json!(text));
        }
        let formats: Vec<String> = (input.supply.formats.iter())
            .map(|term| format!("{EDAM}{}", tool_definition::edam::term_id(term)))
            .collect();
        if !formats.is_empty() {
            keywords.insert("fileFormat".into(), json!(formats));
        }
        if !keywords.is_empty() {
            annotations.insert(name(&input.name), keywords);
        }
        for target in &input.to {
            edges.push(edge(graph_port(&input.name), endpoint(target)));
        }
    }
    let mut entry_outputs = Vec::new();
    for output in workflow.outputs_or_terminal(&tools) {
        let producer = planned
            .iter()
            .find(|plan| plan.path == name(&output.from.node));
        let port = producer.and_then(|plan| {
            (plan.outputs.iter()).find(|port| port.name == name(&output.from.port))
        });
        let Some(port) = port else {
            return Err(LoweringError::Node {
                node: output.from.node.clone(),
                reason: format!(
                    "there is no output {} for the workflow output {}",
                    output.from.port, output.name
                ),
            });
        };
        entry_outputs.push(Port {
            name: name(&output.name),
            r#type: port.r#type.clone(),
            flow: Default::default(),
            default: None,
        });
        if let Some(text) = &output.description {
            let keywords = Map::from_iter([("description".to_owned(), json!(text))]);
            annotations.insert(name(&output.name), keywords);
        }
        edges.push(edge(endpoint(&output.from), graph_port(&output.name)));
    }
    let entry = name(if workflow.name.is_empty() {
        "main"
    } else {
        &workflow.name
    });
    graphs.insert(
        0,
        Graph {
            name: entry.clone(),
            inputs: entry_inputs,
            outputs: entry_outputs,
            ops,
            edges,
        },
    );
    let document = Document {
        wir_version: wir::VERSION.to_owned(),
        sources,
        types: Vec::new(),
        graphs,
        entry,
        hooks: Vec::new(),
        // The formats of the ports are EDAM terms, so a run fits a kind of a wanted format.
        formats: wir::Formats {
            prefixes: Default::default(),
            ontologies: vec![tool_definition::edam::ONTOLOGY.to_owned()],
        },
    };
    if let Err(problems) = wir::verify(&document) {
        let problems = problems.iter().map(ToString::to_string).collect();
        return Err(LoweringError::Invalid(problems));
    }
    Ok(Lowered {
        document,
        files,
        annotations,
    })
}

/// Adds a source unless the same one is there, and gives its id: the id of `source`, with a
/// short digest of its location added when another source has that id already.
fn declare(sources: &mut Vec<Source>, mut source: Source) -> String {
    let same = |known: &Source, source: &Source| {
        (known.language, &known.location, &known.digest)
            == (source.language, &source.location, &source.digest)
    };
    if sources
        .iter()
        .any(|known| known.id == source.id && !same(known, &source))
    {
        let identity = format!(
            "{}\n{}",
            source.location,
            source.digest.as_deref().unwrap_or("")
        );
        let digest = task_model::Digest::sha256(identity.as_bytes());
        source.id = format!("{}-{}", source.id, &digest.hex()[..12]);
    }
    if !sources
        .iter()
        .any(|known| known.id == source.id && same(known, &source))
    {
        sources.push(source.clone());
    }
    source.id
}

/// The ports of each node that an edge or a workflow input feeds, by node id.
fn fed_ports(workflow: &ComposedWorkflow) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut fed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for edge in &workflow.graph.edges {
        fed.entry(&edge.to.node).or_default().insert(&edge.to.port);
    }
    for input in &workflow.inputs {
        for target in &input.to {
            fed.entry(&target.node).or_default().insert(&target.port);
        }
    }
    fed
}

/// A name that wir takes: letters, digits, `_`, `-` and `.`, with `|` of Galaxy paths as `.`.
pub fn name(text: &str) -> String {
    let named: String = (text.chars())
        .map(|c| match c {
            '|' | '/' => '.',
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') => c,
            _ => '_',
        })
        .collect();
    match named.is_empty() {
        true => "_".to_owned(),
        false => named,
    }
}

fn array(item: Type) -> Type {
    Type::Array(ArrayType {
        items: Box::new(item),
        nonempty: false,
    })
}

fn item_of(r#type: &Type) -> Type {
    match r#type {
        Type::Array(array) => (*array.items).clone(),
        other => other.clone(),
    }
}

/// The wir type of a port of a tool definition.
fn port_type(kind: PortKind, cardinality: Cardinality, required: bool) -> Type {
    let item = match kind {
        PortKind::File => Type::File(FileType::default()),
        PortKind::Directory => Type::Directory,
        PortKind::Value(ValueType::Boolean) => Type::Bool,
        PortKind::Value(ValueType::Integer) => Type::Int,
        PortKind::Value(ValueType::Number) => Type::Float,
        PortKind::Value(ValueType::Array) => array(Type::String),
        PortKind::Value(ValueType::String | ValueType::Object) => Type::String,
    };
    let shaped = match cardinality {
        Cardinality::One => item,
        Cardinality::List => array(item),
        Cardinality::Pair => Type::Tuple(vec![item.clone(), item]),
    };
    match required {
        true => shaped,
        false => Type::Optional(Box::new(shaped)),
    }
}

/// The wir type of the values that a JSON Schema describes, when wir can type them; a record
/// field that the schema does not require may be null.
fn schema_type(schema: &Json) -> Option<Type> {
    if let Some(symbols) = schema.get("enum").and_then(Json::as_array) {
        let symbols = symbols
            .iter()
            .map(|symbol| symbol.as_str().map(str::to_owned));
        return Some(Type::Enum(symbols.collect::<Option<_>>()?));
    }
    Some(match schema.get("type")?.as_str()? {
        "boolean" => Type::Bool,
        "integer" => Type::Int,
        "number" => Type::Float,
        "string" => Type::String,
        "array" => array(schema_type(schema.get("items")?)?),
        "object" => {
            let required = schema.get("required").and_then(Json::as_array);
            let required = |name: &str| required.is_some_and(|names| names.contains(&json!(name)));
            let fields = (schema.get("properties")?.as_object()?.iter()).map(|(name, field)| {
                let r#type = schema_type(field)?;
                Some(Field {
                    name: name.clone(),
                    r#type: match required(name) {
                        true => r#type,
                        false => Type::Optional(Box::new(r#type)),
                    },
                })
            });
            Type::Record(fields.collect::<Option<_>>()?)
        }
        _ => return None,
    })
}

/// Sets the format of the File of a type, also inside a list, a pair or an optional value.
fn set_format(r#type: &mut Type, iri: &str) {
    match r#type {
        Type::File(file) => file.format = Some(iri.to_owned()),
        Type::Optional(inner) => set_format(inner, iri),
        Type::Array(array) => set_format(&mut array.items, iri),
        Type::Tuple(items) => items.iter_mut().for_each(|item| set_format(item, iri)),
        _ => {}
    }
}

/// The wir type of a parameter value, from the value itself.
fn json_type(value: &Json) -> Type {
    match value {
        Json::Null => Type::Optional(Box::new(Type::String)),
        Json::Bool(_) => Type::Bool,
        Json::Number(number) if number.is_i64() || number.is_u64() => Type::Int,
        Json::Number(_) => Type::Float,
        Json::String(_) => Type::String,
        Json::Array(items) => array(items.first().map_or(Type::String, json_type)),
        Json::Object(fields) => Type::Record(
            (fields.iter())
                .map(|(key, item)| Field {
                    name: key.clone(),
                    r#type: json_type(item),
                })
                .collect(),
        ),
    }
}

/// The body graph of a map: one op with the ports of the graph, wired through.
fn body_graph(name: &str, op: Op, inputs: &[Port], outputs: &[Port]) -> Graph {
    let path = op.path.clone();
    let mut edges = Vec::new();
    for port in inputs {
        edges.push(Edge {
            from: Endpoint {
                op: None,
                port: port.name.clone(),
            },
            to: Endpoint {
                op: Some(path.clone()),
                port: port.name.clone(),
            },
            read: Default::default(),
        });
    }
    for port in outputs {
        edges.push(Edge {
            from: Endpoint {
                op: Some(path.clone()),
                port: port.name.clone(),
            },
            to: Endpoint {
                op: None,
                port: port.name.clone(),
            },
            read: Default::default(),
        });
    }
    let inputs = (inputs.iter())
        .map(|port| Port {
            default: None,
            ..port.clone()
        })
        .collect();
    Graph {
        name: name.to_owned(),
        inputs,
        outputs: outputs.to_vec(),
        ops: vec![op],
        edges,
    }
}

/// A map op over the inputs `over`, whose outputs are arrays of the round results.
fn map_op(path: &str, body: &str, inputs: &[Port], outputs: &[Port], over: &[String]) -> Op {
    Op {
        path: path.to_owned(),
        kind: Kind::Map(wir::Map {
            body: body.to_owned(),
            over: over.to_vec(),
            mode: Default::default(),
            parallelism: None,
            min_success: None,
        }),
        inputs: inputs.to_vec(),
        outputs: (outputs.iter())
            .map(|port| Port {
                r#type: array(port.r#type.clone()),
                ..port.clone()
            })
            .collect(),
        after: Vec::new(),
        depends: None,
        handles: Vec::new(),
        effects: Vec::new(),
        attributes: Default::default(),
        origin: None,
        values: Default::default(),
    }
}

/// Where a source address is: as it is when absolute or an address, else from the folder of
/// the definition file that names it.
fn located(address: &str, origin: Option<&Path>) -> PathBuf {
    let path = Path::new(address);
    if address.contains("://") || path.is_absolute() {
        return path.to_owned();
    }
    let folder = origin.and_then(Path::parent);
    match folder {
        Some(folder) if !folder.join(path).exists() && path.exists() => path.to_owned(),
        Some(folder) => folder.join(path),
        None => path.to_owned(),
    }
}

/// A location relative to `base` when it lies below it, else absolute.
fn relative(location: &Path, base: &Path) -> String {
    if location.to_string_lossy().contains("://") {
        return location.to_string_lossy().into_owned();
    }
    let absolute = std::path::absolute(location).unwrap_or_else(|_| location.to_owned());
    let base = std::path::absolute(base).unwrap_or_else(|_| base.to_owned());
    match absolute.strip_prefix(&base) {
        Ok(inside) => inside.to_string_lossy().into_owned(),
        Err(_) => absolute.to_string_lossy().into_owned(),
    }
}

/// The entry script of a Nextflow process node: a workflow `node` that calls the process with
/// the op inputs as params, and emits the files of its outputs by name.
fn nextflow_entry(
    module: &Path,
    definition: &Definition,
    ports: &[Port],
) -> Result<String, String> {
    let text = std::fs::read_to_string(module)
        .map_err(|error| format!("cannot read {}: {error}", module.display()))?;
    let parsed = nextflow_parser::parse_script(&text);
    let process = (parsed.tree.items.iter())
        .find_map(|item| match item {
            nextflow_parser::ast::Item::Process(process)
                if process.name.text.eq_ignore_ascii_case(&definition.title)
                    || process.name.text.eq_ignore_ascii_case(&definition.id) =>
            {
                Some(process.as_ref())
            }
            _ => None,
        })
        .ok_or_else(|| format!("{} has no process {}", module.display(), definition.title))?;
    let given = |part: &str| ports.iter().any(|port| port.name == name(part));
    let param = |name: &str| format!("params[{}]", groovy_text(name));
    let mut arguments = Vec::new();
    for parts in super::nextflow::inputs(process, &text) {
        let values: Vec<String> = (parts.iter())
            .map(|part| match part.qualifier.as_str() {
                "val" if part.name == "meta" => "[id: 'node']".to_owned(),
                "path" | "file" if given(&part.name) => format!("file({})", param(&part.name)),
                "path" | "file" => "[]".to_owned(),
                _ => param(&part.name),
            })
            .collect();
        arguments.push(match values.as_slice() {
            [one] if parts.len() == 1 => one.clone(),
            many => format!("channel.of([{}])", many.join(", ")),
        });
    }
    let call = &process.name.text;
    let mut emits = Vec::new();
    for (index, output) in super::nextflow::outputs(process, &text).iter().enumerate() {
        // An output without an emit name is emitted by its place, as the port of the tool.
        let (port, channel) = match &output.emit {
            Some(emit) => (emit.clone(), format!("out.{emit}")),
            None => (format!("output_{}", index + 1), format!("out[{index}]")),
        };
        let port = (definition.outputs.get(index)).map_or(port, |port| port.name.clone());
        let emitted = name(&port);
        if !super::nextflow::identifier(&emitted) {
            return Err(format!("the output {port} is not a Nextflow name"));
        }
        let file = (output.parts.iter())
            .position(|part| matches!(part.qualifier.as_str(), "path" | "file"));
        let picked = match (output.parts.len(), file) {
            (1, _) | (_, None) => String::new(),
            (_, Some(at)) => format!(".map {{ it[{at}] }}"),
        };
        emits.push(format!("    {emitted} = {call}.{channel}{picked}.first()"));
    }
    let include = groovy_text(&module.to_string_lossy());
    Ok(format!(
        "// The entry of the process {call} as one wir op, made by `uwe lower`.\n\
         include {{ {call} }} from {include}\n\n\
         workflow node {{\n    main:\n    {call}({})\n\n    emit:\n{}\n}}\n",
        arguments.join(", "),
        emits.join("\n")
    ))
}

/// A text as a Groovy string in single quotes, with its backslashes, quotes, line breaks and
/// NUL characters escaped.
fn groovy_text(text: &str) -> String {
    let mut quoted = String::from("'");
    for c in text.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '\'' => quoted.push_str("\\'"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\0' => quoted.push_str("\\u0000"),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_types() {
        assert_eq!(name("adv|control_sgrna"), "adv.control_sgrna");
        assert_eq!(name("trim galore"), "trim_galore");
        let list = port_type(PortKind::File, Cardinality::List, false);
        assert_eq!(
            serde_json::to_value(&list).expect("JSON"),
            json!({"optional": {"array": {"items": {"file": {}}}}})
        );
        assert_eq!(groovy_text("a\\b'c\nd\0"), "'a\\\\b\\'c\\nd\\u0000'");
        let shaped =
            schema_type(&json!({"type": "object", "properties": {"lines": {"type": "integer"}}}));
        let lines = Field {
            name: "lines".into(),
            r#type: Type::Optional(Box::new(Type::Int)),
        };
        assert_eq!(
            shaped,
            Some(Type::Record(vec![lines])),
            "an object stays an object"
        );
        assert_eq!(schema_type(&json!({})), None, "any value has no wir type");
        let record = json_type(&json!({"mode": "single", "count": 2, "keep": [0.5]}));
        assert_eq!(
            serde_json::to_value(&record).expect("JSON"),
            json!({"record": [{"name": "mode", "type": "string"}, {"name": "count", "type": "int"},
                {"name": "keep", "type": {"array": {"items": "float"}}}]})
        );
    }
}
