//! Galaxy workflows of `.ga` files: their inputs, tool steps, connections and outputs, and the
//! tool files that the steps name.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json};

/// A Galaxy workflow: its inputs, and its tool steps in the order of their ids.
#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub annotation: Option<String>,
    pub inputs: Vec<WorkflowInput>,
    pub steps: Vec<Step>,
    pub outputs: Vec<WorkflowOutput>,
}

/// An input step: a dataset, a collection or a parameter, by its label.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowInput {
    pub id: u64,
    pub label: String,
    pub kind: InputKind,
    pub optional: bool,
    pub default: Option<Json>,
    /// The Galaxy datatypes a dataset input takes; empty takes any.
    pub formats: Vec<String>,
    pub annotation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    Dataset,
    /// A list of datasets, as a flat list collection.
    Collection,
    /// A value of a parameter type, like `integer` or `text`.
    Parameter(String),
}

/// A tool step with its tool state and the connections of its inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: u64,
    pub label: Option<String>,
    pub tool_id: String,
    pub tool_version: Option<String>,
    /// The parameter values of the step, nested like the tool inputs, without connected ones.
    pub state: Map<String, Json>,
    pub connections: Vec<Connection>,
}

/// The sources of one input of a step, by its path like `adv|control`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub input: String,
    /// Each source as a step id and the name of its output.
    pub sources: Vec<(u64, String)>,
}

/// An output of a step that the workflow gives, by its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowOutput {
    pub label: String,
    pub step: u64,
    pub output: String,
}

/// Whether a text is a Galaxy workflow: JSON with `a_galaxy_workflow`.
pub fn is_workflow(text: &str) -> bool {
    let starts = text.trim_start().starts_with('{');
    starts && text.contains("a_galaxy_workflow") && workflow_json(text).is_some()
}

fn workflow_json(text: &str) -> Option<Map<String, Json>> {
    let Ok(Json::Object(object)) = serde_json::from_str::<Json>(text) else {
        return None;
    };
    let flag = object.get("a_galaxy_workflow")?;
    let set = flag == "true" || *flag == Json::Bool(true);
    set.then_some(object)
}

/// Whether a text is a Galaxy tool: XML whose root element is `<tool>`.
pub fn is_tool(text: &str) -> bool {
    let mut rest = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("<?") {
            rest = after
                .split_once("?>")
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else if let Some(after) = rest.strip_prefix("<!--") {
            rest = after
                .split_once("-->")
                .map_or("", |(_, tail)| tail)
                .trim_start();
        } else {
            break;
        }
    }
    let Some(element) = rest.strip_prefix("<tool") else {
        return false;
    };
    element.starts_with([' ', '>', '\n', '\t', '\r', '/'])
}

/// Reads a workflow from the text of a `.ga` file.
pub fn parse(text: &str) -> Result<Workflow, String> {
    let object = workflow_json(text).ok_or("not a Galaxy workflow: no a_galaxy_workflow")?;
    let text_of = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_owned);
    let mut steps: Vec<(u64, &Map<String, Json>)> = Vec::new();
    for (key, step) in (object.get("steps").and_then(Json::as_object))
        .into_iter()
        .flatten()
    {
        let Json::Object(step) = step else {
            return Err(format!("step {key} is not an object"));
        };
        let id = (step.get("id").and_then(Json::as_u64))
            .or_else(|| key.parse().ok())
            .ok_or_else(|| format!("step {key} has no id"))?;
        steps.push((id, step));
    }
    steps.sort_by_key(|(id, _)| *id);
    if let Some(pair) = steps.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(format!("two steps have the id {}", pair[0].0));
    }
    let mut workflow = Workflow {
        name: text_of("name").unwrap_or_default(),
        annotation: text_of("annotation").filter(|text| !text.is_empty()),
        inputs: Vec::new(),
        steps: Vec::new(),
        outputs: Vec::new(),
    };
    for (id, step) in steps {
        let kind = step.get("type").and_then(Json::as_str).unwrap_or("tool");
        let state = state(step.get("tool_state")).map_err(|why| format!("step {id}: {why}"))?;
        let label = (step.get("label").and_then(Json::as_str))
            .filter(|label| !label.is_empty())
            .map(str::to_owned);
        for output in (step.get("workflow_outputs").and_then(Json::as_array))
            .into_iter()
            .flatten()
        {
            let name = output
                .get("output_name")
                .and_then(Json::as_str)
                .unwrap_or("output");
            let label = (output.get("label").and_then(Json::as_str))
                .filter(|label| !label.is_empty())
                .map_or_else(|| format!("{id}.{name}"), str::to_owned);
            workflow.outputs.push(WorkflowOutput {
                label,
                step: id,
                output: name.to_owned(),
            });
        }
        let input = |kind: InputKind| {
            let name = (step.get("inputs").and_then(Json::as_array))
                .and_then(|inputs| inputs.first())
                .and_then(|input| input.get("name").and_then(Json::as_str))
                .map(str::to_owned);
            let formats = match state.get("format") {
                Some(Json::Array(items)) => (items.iter())
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            WorkflowInput {
                id,
                label: label.clone().or(name).unwrap_or_else(|| id.to_string()),
                kind,
                optional: state.get("optional") == Some(&Json::Bool(true)),
                default: state
                    .get("default")
                    .filter(|value| !value.is_null())
                    .cloned(),
                formats,
                annotation: (step.get("annotation").and_then(Json::as_str))
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
            }
        };
        match kind {
            "data_input" => workflow.inputs.push(input(InputKind::Dataset)),
            "data_collection_input" => workflow.inputs.push(input(InputKind::Collection)),
            "parameter_input" => {
                let kind = state.get("parameter_type").and_then(Json::as_str);
                let kind = InputKind::Parameter(kind.unwrap_or("text").to_owned());
                workflow.inputs.push(input(kind));
            }
            "tool" => workflow.steps.push(Step {
                id,
                label,
                tool_id: (step.get("tool_id").and_then(Json::as_str))
                    .ok_or_else(|| format!("step {id} names no tool"))?
                    .to_owned(),
                tool_version: step
                    .get("tool_version")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
                state,
                connections: connections(step.get("input_connections")),
            }),
            other => {
                return Err(format!(
                    "step {id}: steps of type {other} are not supported yet"
                ));
            }
        }
    }
    Ok(workflow)
}

/// The tool state of a step: JSON text, or an object, whose values may be JSON text again,
/// as older Galaxy versions encode each one. A state that is not an object is refused.
fn state(given: Option<&Json>) -> Result<Map<String, Json>, String> {
    let decoded = match given {
        Some(Json::String(text)) => serde_json::from_str(text)
            .map_err(|error| format!("the tool_state is not valid JSON: {error}"))?,
        Some(other) => other.clone(),
        None => Json::Null,
    };
    let object = match decoded {
        Json::Object(object) => object,
        Json::Null => return Ok(Map::new()),
        _ => return Err("the tool_state is not an object".to_owned()),
    };
    Ok((object.into_iter())
        .filter(|(key, _)| !key.starts_with("__") && key != "chromInfo")
        .filter_map(|(key, value)| Some((key, decoded_value(value)?)))
        .collect())
}

/// A value of the tool state with its JSON text decoded; none for a value that a connection
/// or the run gives, which Galaxy marks with `__class__`.
fn decoded_value(value: Json) -> Option<Json> {
    match value {
        Json::String(text) if text.starts_with(['{', '[', '"']) || text == "null" => {
            match serde_json::from_str(&text) {
                Ok(decoded) => decoded_value(decoded),
                Err(_) => Some(Json::String(text)),
            }
        }
        Json::Object(object) if object.contains_key("__class__") => None,
        Json::Object(object) => Some(Json::Object(
            (object.into_iter())
                .filter(|(key, _)| !key.starts_with("__"))
                .filter_map(|(key, value)| Some((key, decoded_value(value)?)))
                .collect(),
        )),
        Json::Array(items) => Some(Json::Array(
            items.into_iter().filter_map(decoded_value).collect(),
        )),
        other => Some(other),
    }
}

fn connections(given: Option<&Json>) -> Vec<Connection> {
    let Some(Json::Object(given)) = given else {
        return Vec::new();
    };
    let source = |link: &Json| {
        let id = link.get("id").and_then(Json::as_u64)?;
        let output = link
            .get("output_name")
            .and_then(Json::as_str)
            .unwrap_or("output");
        Some((id, output.to_owned()))
    };
    (given.iter())
        .map(|(input, links)| Connection {
            input: input.clone(),
            sources: match links {
                Json::Array(links) => links.iter().filter_map(source).collect(),
                link => source(link).into_iter().collect(),
            },
        })
        .collect()
}

/// The short id of a tool id: the tool of a Tool Shed id like
/// `toolshed.g2.bx.psu.edu/repos/iuc/get_hrun/get_hrun/1.0`, else the id itself.
pub fn short_id(id: &str) -> &str {
    let parts: Vec<&str> = id.split('/').collect();
    match parts.as_slice() {
        [_, "repos", _, _, tool, ..] => tool,
        _ => id,
    }
}

/// The tool file of a step: an XML file below one of `folders` whose tool has the id after its
/// macros are expanded, at exactly the version when one is given, else the first in path order.
pub fn find_tool(id: &str, version: Option<&str>, folders: &[PathBuf]) -> Result<PathBuf, String> {
    let wanted = short_id(id);
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for folder in folders {
        let mut files = Vec::new();
        xml_files(folder, 0, &mut files);
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !is_tool(&text) {
                continue;
            }
            let start = text.find("<tool").unwrap_or_default();
            let tag = &text[start..];
            let head = tag.split_once('>').map_or(tag, |(head, _)| head);
            // Only a literal id that differs rules a file out before its macros are expanded.
            if attribute(head, "id").is_some_and(|known| known != wanted) {
                continue;
            }
            let tool = match galaxy_parser::tool::load(&path) {
                Ok(tool) => tool,
                // A file that names the tool literally reports why it does not load.
                Err(error) if attribute(head, "id").as_deref() == Some(wanted) => {
                    return Err(format!("{}: {error}", path.display()));
                }
                Err(_) => continue,
            };
            if tool.id == wanted {
                found.push((path, tool.version));
            }
        }
    }
    let chosen = match version {
        Some(version) => found.iter().find(|(_, known)| known == version),
        None => found.first(),
    };
    chosen.map(|(path, _)| path.clone()).ok_or_else(|| {
        let shown: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
        let versions: Vec<&str> = found.iter().map(|(_, known)| known.as_str()).collect();
        match (version, versions.is_empty()) {
            (Some(version), false) => format!(
                "no tool file of {wanted} at version {version} below {}; it has the versions {}",
                shown.join(", "),
                versions.join(", ")
            ),
            _ => format!("no tool file of {wanted} below {}", shown.join(", ")),
        }
    })
}

/// The value of an attribute in the text of a start tag, when it is literal.
fn attribute(head: &str, name: &str) -> Option<String> {
    let at = head
        .find(&format!(" {name}="))
        .or_else(|| head.find(&format!("\n{name}=")))
        .or_else(|| head.find(&format!("\t{name}=")))?;
    let rest = &head[at + name.len() + 2..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = rest[1..].split(quote).next()?;
    (!value.contains('@')).then(|| value.to_owned())
}

/// The XML files below a folder, four levels deep at most, without hidden folders.
fn xml_files(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if path.is_dir() && !hidden && depth < 4 {
            xml_files(&path, depth + 1, found);
        } else if path.extension().is_some_and(|ending| ending == "xml") {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_steps() {
        let text = json!({
            "a_galaxy_workflow": "true", "name": "w",
            "steps": {
                "1": {"id": 1, "type": "tool", "tool_id": "toolshed.g2.bx.psu.edu/repos/iuc/x/x/1.0",
                      "tool_state": "{\"count\": \"\\\"3\\\"\", \"in\": \"{\\\"__class__\\\": \\\"ConnectedValue\\\"}\", \"__page__\": null}",
                      "input_connections": {"in": {"id": 0, "output_name": "output"}},
                      "workflow_outputs": [{"output_name": "out", "label": "result"}]},
                "0": {"id": 0, "type": "data_input", "label": "reads", "tool_state": "{\"optional\": false}"}
            }
        })
        .to_string();
        assert!(is_workflow(&text));
        let workflow = parse(&text).expect("a workflow");
        assert_eq!(workflow.inputs[0].label, "reads");
        let step = &workflow.steps[0];
        assert_eq!(short_id(&step.tool_id), "x");
        assert_eq!(Json::Object(step.state.clone()), json!({"count": "3"}));
        assert_eq!(step.connections[0].sources, [(0, "output".to_owned())]);
        assert_eq!(workflow.outputs[0].label, "result");
    }

    #[test]
    fn refuses_broken_workflows() {
        let broken = json!({"a_galaxy_workflow": "true", "steps": {
            "1": {"id": 1, "type": "tool", "tool_id": "x", "tool_state": "{\"count\": "}}});
        let error = parse(&broken.to_string()).expect_err("a broken state");
        assert!(error.contains("not valid JSON"), "{error}");
        let twice = json!({"a_galaxy_workflow": "true", "steps": {
            "1": {"id": 1, "type": "tool", "tool_id": "x"},
            "2": {"id": 1, "type": "tool", "tool_id": "y"}}});
        let error = parse(&twice.to_string()).expect_err("two steps of one id");
        assert!(error.contains("two steps have the id 1"), "{error}");
    }

    #[test]
    fn finds_exact_versions() {
        let folder = tempfile::tempdir().expect("a folder");
        let tool = |version: &str| {
            format!(
                "<tool id=\"t\" version=\"@V@\"><macros><token name=\"@V@\">{version}</token>\
                 </macros><command>x</command></tool>"
            )
        };
        std::fs::write(folder.path().join("a.xml"), tool("1.0")).expect("a tool");
        std::fs::write(folder.path().join("b.xml"), tool("2.0")).expect("a tool");
        let folders = [folder.path().to_owned()];
        let found = find_tool("t", Some("2.0"), &folders).expect("the second version");
        assert_eq!(found, folder.path().join("b.xml"));
        let first = find_tool("t", None, &folders).expect("the first file");
        assert_eq!(first, folder.path().join("a.xml"));
        let error = find_tool("t", Some("3.0"), &folders).expect_err("no such version");
        assert!(
            error.contains("at version 3.0") && error.contains("1.0, 2.0"),
            "{error}"
        );
    }

    #[test]
    fn tells_tools() {
        assert!(is_tool(
            "<?xml version=\"1.0\"?>\n<!-- c -->\n<tool id=\"a\">"
        ));
        assert!(!is_tool("<toolbox>"));
        assert!(!is_tool("{\"a\": 1}"));
        assert_eq!(
            attribute(r#" id="get_hrun" version="1.0""#, "version").as_deref(),
            Some("1.0")
        );
    }
}
