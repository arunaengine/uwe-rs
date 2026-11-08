//! Expr ops in WDL: a WDL expression or command section over the input values of an op, run
//! by the evaluator and standard library of uwe's WDL driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use wdl_analysis::types::v1::AstTypeConverter;
use wdl_analysis::types::{Type as Declared, TypeNameResolver};
use wdl_ast::v1::{BoundDecl, CommandSection, WorkflowStatement};
use wdl_ast::{AstToken, Diagnostic, Severity, Span};
use wdl_runtime::Value;
use wdl_runtime::expression::{self, Environment};
use wir::{Document, KeyType, Op, Type, TypeDef, resolve};

/// The WDL version that code is parsed as when its op names no WDL source.
const VERSION: &str = "1.2";

/// The value of the output of a WDL expr op from its input values, or why it has none.
pub fn evaluate(
    document: &Document,
    op: &Op,
    spec: &wir::Expr,
    bound: &Map<String, Json>,
) -> Result<Json, String> {
    let version = version(document, op);
    let defs = &document.types;
    let command = spec.code.trim_start().starts_with("command");
    let mut scope = Scope::default();
    for port in &op.inputs {
        let json = bound.get(&port.name).unwrap_or(&Json::Null);
        // A command runs in a task op, which stages the files of input `p` below `inputs/p`.
        let folder = command.then(|| format!("inputs/{}", port.name));
        let value = to_wdl(
            json,
            &port.r#type,
            defs,
            folder.as_deref(),
            &mut scope.files,
        );
        scope.names.insert(port.name.clone(), value);
    }
    for line in &spec.library {
        let decl = declaration(line, version)?;
        let name = decl.name().text().to_owned();
        let ty = declared(&decl.ty())?;
        let value = expression::evaluate(&decl.expr(), &mut scope)
            .map_err(|error| format!("`{name}`: {error}"))?;
        let value = value
            .coerce(&ty)
            .map_err(|error| format!("`{name}`: {error}"))?;
        scope.names.insert(name, value);
    }
    let value = match command {
        true => {
            let section = section(&spec.code, version)?;
            Value::String(expression::command(&section, &mut scope).map_err(|e| e.to_string())?)
        }
        false => {
            let decl = declaration(&format!("String value = {}", spec.code), version)?;
            expression::evaluate(&decl.expr(), &mut scope).map_err(|error| error.to_string())?
        }
    };
    let output = &op.outputs[0];
    let json = to_wir(value, &output.r#type, defs, &scope.files)?;
    match wir::fits(&json, &output.r#type, defs) {
        true => Ok(json),
        false => Err(format!(
            "the value {json} does not fit output {}",
            output.name
        )),
    }
}

/// The WDL version of the source that the op comes from.
fn version<'d>(document: &'d Document, op: &Op) -> &'d str {
    let id = op.origin.as_ref().map(|origin| origin.source.as_str());
    let source = (document.sources.iter()).find(|source| Some(source.id.as_str()) == id);
    let wdl = source.filter(|source| source.language == wir::Language::Wdl);
    wdl.and_then(|source| source.version.as_deref())
        .unwrap_or(VERSION)
}

/// The names that an expression sees, and the wir values of the files among them by path.
#[derive(Default)]
struct Scope {
    names: BTreeMap<String, Value>,
    files: HashMap<String, Json>,
}

impl Environment for Scope {
    /// A name, or an object of the inputs `name.<member>`, as WDL sees the outputs of a call.
    fn lookup(&self, name: &str) -> Option<Value> {
        if let Some(value) = self.names.get(name) {
            return Some(value.clone());
        }
        let prefix = format!("{name}.");
        let members: IndexMap<String, Value> = (self.names.iter())
            .filter_map(|(key, value)| Some((key.strip_prefix(&prefix)?.to_owned(), value.clone())))
            .collect();
        (!members.is_empty()).then_some(Value::Object(members))
    }
}

/// A WDL document of `version` with one item, and the first syntax error in it.
fn parsed(item: &str, version: &str) -> Result<wdl_ast::v1::Ast, String> {
    let text = format!("version {version}\n{item}\n");
    let (parsed, diagnostics) = wdl_ast::Document::parse(&text, None);
    if let Some(error) = (diagnostics.iter()).find(|item| item.severity() == Severity::Error) {
        return Err(format!("the WDL code does not parse: {}", error.message()));
    }
    match parsed.ast() {
        wdl_ast::Ast::V1(ast) => Ok(ast),
        wdl_ast::Ast::Unsupported => Err(format!("WDL {version} is not supported")),
    }
}

/// A declaration like `Int n = 1`.
fn declaration(text: &str, version: &str) -> Result<BoundDecl, String> {
    let ast = parsed(&format!("workflow expr {{\n{text}\n}}"), version)?;
    let statement = ast
        .workflows()
        .next()
        .and_then(|item| item.statements().next());
    match statement {
        Some(WorkflowStatement::Declaration(decl)) => Ok(decl),
        _ => Err(format!("`{text}` is not one WDL declaration")),
    }
}

fn section(text: &str, version: &str) -> Result<CommandSection, String> {
    let ast = parsed(&format!("task expr {{\n{text}\n}}"), version)?;
    (ast.tasks().next().and_then(|task| task.command()))
        .ok_or_else(|| "the code is not a WDL command section".to_owned())
}

/// The type of a declaration. Structs have no definitions here, so their values stay as
/// they are.
fn declared(ty: &wdl_ast::v1::Type) -> Result<Declared, String> {
    struct Open;

    impl TypeNameResolver for Open {
        fn resolve(&mut self, _: &str, _: Span) -> Result<Declared, Diagnostic> {
            Ok(Declared::Union)
        }
    }

    (AstTypeConverter::new(Open).convert_type(ty))
        .map_err(|diagnostic| diagnostic.message().to_owned())
}

/// A wir value as a WDL value. A File or Directory is its local path or address, or its path
/// below `folder` where a task op stages it, as `inputs/<port>/<index>/<basename>`.
fn to_wdl(
    json: &Json,
    r#type: &Type,
    defs: &[TypeDef],
    folder: Option<&str>,
    files: &mut HashMap<String, Json>,
) -> Value {
    let resolved = resolve(r#type, defs).unwrap_or(&Type::Dynamic);
    let item = |index: usize| folder.map(|folder| format!("{folder}/{index}"));
    match (resolved, json) {
        (_, Json::Null) => Value::None,
        (Type::Optional(inner), _) => to_wdl(json, inner, defs, folder, files),
        (_, Json::Object(object)) if entry(object).is_some() => {
            let (class, location) = entry(object).unwrap_or_default();
            let path = match folder {
                Some(folder) => format!("{folder}/{}", basename(object, location)),
                None => cwl_parser::iri::file_path(location).map_or(location.to_owned(), |path| {
                    path.to_string_lossy().into_owned()
                }),
            };
            files.insert(path.clone(), json.clone());
            match class {
                "Directory" => Value::Directory(path),
                _ => Value::File(path),
            }
        }
        (Type::Tuple(types), Json::Array(items)) if items.len() == 2 && types.len() == 2 => {
            let left = to_wdl(&items[0], &types[0], defs, item(0).as_deref(), files);
            let right = to_wdl(&items[1], &types[1], defs, item(1).as_deref(), files);
            Value::Pair(Box::new(left), Box::new(right))
        }
        (_, Json::Array(items)) => {
            let element = match resolved {
                Type::Array(array) => &*array.items,
                _ => &Type::Dynamic,
            };
            let values = (items.iter().enumerate())
                .map(|(index, json)| to_wdl(json, element, defs, item(index).as_deref(), files));
            Value::Array(values.collect())
        }
        (Type::Map(map), Json::Object(object)) => Value::Map(
            (object.iter())
                .map(|(key, json)| {
                    (
                        map_key(key, map.key),
                        to_wdl(json, &map.value, defs, None, files),
                    )
                })
                .collect(),
        ),
        (_, Json::Object(object)) => {
            let field = |name: &str| match resolved {
                Type::Record(fields) => (fields.iter().find(|field| field.name == name))
                    .map_or(Type::Dynamic, |field| field.r#type.clone()),
                _ => Type::Dynamic,
            };
            Value::Object(
                (object.iter())
                    .map(|(key, json)| (key.clone(), to_wdl(json, &field(key), defs, None, files)))
                    .collect(),
            )
        }
        (_, Json::Bool(flag)) => Value::Boolean(*flag),
        (Type::Float, Json::Number(number)) => Value::Float(number.as_f64().unwrap_or_default()),
        (_, Json::Number(number)) => match number.as_i64() {
            Some(whole) => Value::Int(whole),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        (_, Json::String(text)) => Value::String(text.clone()),
    }
}

/// The class and location of a File or Directory value.
fn entry(object: &Map<String, Json>) -> Option<(&str, &str)> {
    let class = object.get("class").and_then(Json::as_str)?;
    let location = object
        .get("location")
        .and_then(Json::as_str)
        .unwrap_or_default();
    matches!(class, "File" | "Directory").then_some((class, location))
}

/// The name of a staged file, as a task op picks it: its basename, else the end of its
/// location.
fn basename<'a>(object: &'a Map<String, Json>, location: &'a str) -> &'a str {
    (object.get("basename").and_then(Json::as_str))
        .or_else(|| location.trim_end_matches('/').rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("input")
}

fn map_key(text: &str, key: KeyType) -> Value {
    let fallback = || Value::String(text.into());
    match key {
        KeyType::Int => text.parse().map_or_else(|_| fallback(), Value::Int),
        KeyType::Bool => text.parse().map_or_else(|_| fallback(), Value::Boolean),
        KeyType::String => fallback(),
    }
}

/// A WDL value as a wir value of a type, coerced as WDL coerces to a declared type. A File
/// that came in keeps its wir value.
fn to_wir(
    value: Value,
    r#type: &Type,
    defs: &[TypeDef],
    files: &HashMap<String, Json>,
) -> Result<Json, String> {
    let shown = serde_json::to_string(r#type).unwrap_or_default();
    let fail = |value: &Value| format!("a {} does not fit {shown}", value.kind());
    let resolved = resolve(r#type, defs).unwrap_or(&Type::Dynamic);
    Ok(match (resolved, value) {
        (_, Value::None) => Json::Null,
        (Type::Optional(inner), value) => to_wir(value, inner, defs, files)?,
        (
            Type::File(_) | Type::Directory,
            Value::File(path) | Value::String(path) | Value::Directory(path),
        ) => {
            let class = match resolved {
                Type::Directory => "Directory",
                _ => "File",
            };
            located(class, path, files)
        }
        (Type::String | Type::Enum(_), value) => match value.text() {
            Some(text) => Json::String(text),
            None => return Err(fail(&value)),
        },
        (Type::Float, Value::Int(number)) => json!(number as f64),
        (Type::Array(array), Value::Array(items)) => Json::Array(
            (items.into_iter())
                .map(|item| to_wir(item, &array.items, defs, files))
                .collect::<Result<_, _>>()?,
        ),
        (Type::Tuple(types), Value::Pair(left, right)) if types.len() == 2 => Json::Array(vec![
            to_wir(*left, &types[0], defs, files)?,
            to_wir(*right, &types[1], defs, files)?,
        ]),
        (Type::Map(map), Value::Map(entries)) => {
            let mut object = Map::new();
            for (key, item) in entries {
                let key = key.text().ok_or_else(|| fail(&key))?;
                object.insert(key, to_wir(item, &map.value, defs, files)?);
            }
            Json::Object(object)
        }
        (Type::Map(_) | Type::Record(_), Value::Object(fields)) => {
            let entries = fields
                .into_iter()
                .map(|(name, item)| (Value::String(name), item));
            record(resolved, entries.collect(), defs, files)?
        }
        (Type::Record(_), Value::Map(entries)) => record(resolved, entries, defs, files)?,
        (Type::Dynamic | Type::Union(_), value) => loose(value, files),
        (_, value) => {
            let json = loose(value, files);
            match wir::fits(&json, r#type, defs) {
                true => json,
                false => return Err(format!("the value {json} does not fit {shown}")),
            }
        }
    })
}

/// A record, or a map with text keys, from WDL entries with text keys. A record has every
/// field, null where the entries have none, as WDL coerces a struct.
fn record(
    r#type: &Type,
    entries: Vec<(Value, Value)>,
    defs: &[TypeDef],
    files: &HashMap<String, Json>,
) -> Result<Json, String> {
    let mut object = Map::new();
    for (key, item) in entries {
        let name = key.text().ok_or("a map key is not text")?;
        let want = match r#type {
            Type::Record(fields) => (fields.iter().find(|field| field.name == name))
                .map_or(Type::Dynamic, |field| field.r#type.clone()),
            Type::Map(map) => (*map.value).clone(),
            _ => Type::Dynamic,
        };
        object.insert(name, to_wir(item, &want, defs, files)?);
    }
    if let Type::Record(fields) = r#type {
        let mut ordered: Map<String, Json> = (fields.iter())
            .map(|field| {
                (
                    field.name.clone(),
                    object.remove(&field.name).unwrap_or_default(),
                )
            })
            .collect();
        ordered.extend(object);
        object = ordered;
    }
    Ok(Json::Object(object))
}

/// A WDL value as wir JSON without a type to follow: a pair as an array of two.
fn loose(value: Value, files: &HashMap<String, Json>) -> Json {
    match value {
        Value::File(path) => located("File", path, files),
        Value::Directory(path) => located("Directory", path, files),
        Value::Array(items) => {
            Json::Array(items.into_iter().map(|item| loose(item, files)).collect())
        }
        Value::Pair(left, right) => Json::Array(vec![loose(*left, files), loose(*right, files)]),
        Value::Map(entries) => Json::Object(
            (entries.into_iter())
                .map(|(key, item)| (key.text().unwrap_or_default(), loose(item, files)))
                .collect(),
        ),
        Value::Object(fields) => Json::Object(
            (fields.into_iter())
                .map(|(name, item)| (name, loose(item, files)))
                .collect(),
        ),
        other => other.to_json(),
    }
}

/// The wir value of a file at a path: the one that came in, else a new File or Directory.
fn located(class: &str, path: String, files: &HashMap<String, Json>) -> Json {
    if let Some(known) = files.get(&path) {
        return known.clone();
    }
    let trimmed = path.trim_end_matches('/');
    let basename = trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned();
    json!({"class": class, "location": path, "basename": basename})
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value as Json, json};

    use super::evaluate;

    /// The value of an expr op in WDL with typed inputs and a library.
    fn run(code: &str, library: &[&str], inputs: Json, output: Json) -> Result<Json, String> {
        let mut ports = Vec::new();
        let mut bound = Map::new();
        for (name, item) in inputs.as_object().cloned().unwrap_or_default() {
            ports.push(json!({"name": name, "type": item["type"]}));
            bound.insert(name, item["value"].clone());
        }
        let op = json!({
            "path": "e",
            "kind": {"expr": {"language": "wdl", "code": code, "library": library}},
            "inputs": ports,
            "outputs": [{"name": "value", "type": output}],
        });
        let document = json!({"wir_version": "0.1", "graphs": [{"name": "main", "ops": [op]}], "entry": "main"});
        let document: wir::Document = serde_json::from_value(document).expect("a document");
        let op = &document.graphs[0].ops[0];
        let wir::Kind::Expr(spec) = &op.kind else {
            panic!("an expr op");
        };
        evaluate(&document, op, spec, &bound)
    }

    fn file(location: &str) -> Json {
        let basename = location.rsplit('/').next().unwrap_or_default();
        json!({"class": "File", "location": location, "basename": basename})
    }

    #[test]
    fn library_coerces_values() {
        let inputs = json!({"n": {"type": "int", "value": 2}});
        let library = ["Float half = n / 2", "Float x = n"];
        let text = run("\"~{x} ~{half}\"", &library, inputs, json!("string"));
        assert_eq!(text, Ok(json!("2.000000 1.000000")));
    }

    #[test]
    fn commands_see_staging() {
        let files = json!({"array": {"items": {"file": {}}}});
        let inputs = json!({
            "reads": {"type": {"file": {}}, "value": file("/data/a.txt")},
            "more": {"type": files, "value": [file("/data/b.txt"), file("/data/c.txt")]},
            "label": {"type": {"optional": "string"}, "value": null},
        });
        let code = "command <<<\n  cat ~{reads} ~{sep=' ' more} ~{default='none' label}\n>>>";
        let text = run(code, &[], inputs, json!("string"));
        let staged = "cat inputs/reads/a.txt inputs/more/0/b.txt inputs/more/1/c.txt none";
        assert_eq!(text, Ok(json!(staged)));
    }

    #[test]
    fn dotted_inputs_nest() {
        let inputs = json!({
            "align.bam": {"type": {"file": {}}, "value": file("/w/x.bam")},
            "align.count": {"type": "int", "value": 3},
        });
        let text = run(
            "basename(align.bam) + align.count",
            &[],
            inputs,
            json!("string"),
        );
        assert_eq!(text, Ok(json!("x.bam3")));
    }

    #[test]
    fn files_keep_values() {
        let mut known = file("/w/x.txt");
        known["size"] = json!(5);
        let inputs = json!({"f": {"type": {"file": {}}, "value": known}});
        let output = run("f", &[], inputs.clone(), json!({"file": {}}));
        assert_eq!(output, Ok(known));
        let made = run("\"out/y.txt\"", &[], inputs, json!({"file": {}}));
        assert_eq!(made, Ok(file("out/y.txt")));
    }

    #[test]
    fn values_take_types() {
        let pairs = json!({"array": {"items": {"tuple": ["int", "string"]}}});
        let zipped = run("zip([1, 2], [\"a\", \"b\"])", &[], json!({}), pairs);
        assert_eq!(zipped, Ok(json!([[1, "a"], [2, "b"]])));
        let map = json!({"map": {"key": "string", "value": "float"}});
        let counts = run("{\"a\": 1, \"b\": 2.5}", &[], json!({}), map);
        assert_eq!(counts, Ok(json!({"a": 1.0, "b": 2.5})));
    }

    #[test]
    fn records_have_fields() {
        let fields =
            json!([{"name": "a", "type": "int"}, {"name": "b", "type": {"optional": "string"}}]);
        let output = json!({"record": fields});
        let made = run("object { a: 1 }", &[], json!({}), output);
        assert_eq!(made, Ok(json!({"a": 1, "b": null})));
    }

    #[test]
    fn failures_are_errors() {
        let text = run("1 / 0", &[], json!({}), json!("int"));
        assert!(text.is_err_and(|why| why.contains("division by zero")));
        let unread = run("read_string(\"a.txt\")", &[], json!({}), json!("string"));
        assert!(unread.is_err(), "an expr reads no files");
        let wrong = run("\"text\"", &[], json!({}), json!("int"));
        assert!(wrong.is_err(), "a string does not fit an int");
    }
}
