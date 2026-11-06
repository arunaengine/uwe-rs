//! Agent tasks in the network: the refusals before a run starts, the model call that a task
//! asks the driver for, and the binding of a structured answer to the agent's output.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, TaskDirective};
use nextflow_evaluator::lowered::{Agent, AnswerType, Input, Process, Structured};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{MapKind, PathValue, Value};
use nextflow_plugins::nf_agent;
use serde_json::Value as Json;

/// What the driver needs to ask the model for one agent task.
#[derive(Debug)]
pub struct Call {
    /// The name of the agent, for messages.
    pub agent: String,
    /// The model id, like `openai/gpt-5-mini`.
    pub model: String,
    pub system: String,
    pub user: String,
    /// The JSON schema of a structured answer.
    pub schema: Option<Json>,
    pub max_iterations: i64,
}

/// The default cap of the tool loop, which keys the cache of a run without tools too.
const MAX_ITERATIONS: i64 = 20;

/// Without its own `maxForks`, an agent runs at most this many tasks at a time.
pub(crate) const FORKS: usize = 10;

/// Refuses what uwe does not run yet, while the workflow is built, before any task starts.
pub(crate) fn refuse(process: &Process, config: &Config) -> Result<(), Error> {
    let unsupported = |message: String| {
        Err(Error::Unsupported {
            message,
            span: process.span,
        })
    };
    if let Some(message) = nf_agent::refusal(config) {
        return unsupported(message);
    }
    for directive in &process.directives {
        let missing = match &*directive.name {
            "tools" => "the tool-calling loop with module, file and shell tools",
            "skills" => "the loading of skills, `SKILL.md` folders with the `activate_skill` tool",
            _ => continue,
        };
        return unsupported(format!(
            "the `{}` directive of agent `{}`: uwe sends one model request per agent task, \
             without {missing}",
            directive.name, process.name
        ));
    }
    Ok(())
}

/// The model call of one agent task, from its directives, its inputs and its prompt.
pub(crate) fn call(
    process: &Process,
    agent: &Agent,
    directives: &[TaskDirective],
    values: &[Value],
    staged: &[(PathValue, String)],
    prompt: &str,
) -> Result<Call, Error> {
    let text = |name: &str| {
        (directives.iter().rev())
            .find(|directive| &*directive.name == name)
            .and_then(|directive| directive.arguments.first())
            .filter(|value| !matches!(value, Value::Null))
            .map(Value::to_string)
    };
    let Some(model) = text("model") else {
        return Err(failed("Agent `model` directive is required".into()));
    };
    if let Some(executor) = text("executor").filter(|executor| executor != "local") {
        return Err(failed(format!(
            "Agent runner `langchain4j` does not support executor `{executor}`; only \
             canonical launch-spec runners can be offloaded"
        )));
    }
    let max_iterations =
        (text("maxIterations").and_then(|value| value.parse().ok())).unwrap_or(MAX_ITERATIONS);
    let instruction = text("instruction").unwrap_or_default();
    let input = input_json(process, agent, values, staged);
    Ok(Call {
        agent: process.name.to_string(),
        model,
        system: nf_agent::system(&instruction, &text("goal").unwrap_or_default()),
        user: nf_agent::user(prompt, input.as_ref()),
        schema: agent.structured.as_ref().map(nf_agent::schema),
        max_iterations,
    })
}

/// The inputs as the model sees them: the value of the one input, or an object of the
/// values by name; staged files show their staged names.
fn input_json(
    process: &Process,
    agent: &Agent,
    values: &[Value],
    staged: &[(PathValue, String)],
) -> Option<Json> {
    let mut entries: Vec<(&str, Value)> = Vec::new();
    let inputs = process.inputs.iter().zip(&agent.inputs).zip(values);
    for ((input, names), value) in inputs {
        match (input, value) {
            (Input::Record(_), Value::Map(map)) => {
                for name in names {
                    let field = map.borrow().entries.get(&key(name)).cloned();
                    entries.push((name, field.unwrap_or(Value::Null)));
                }
            }
            (Input::Tuple(_), Value::List(list)) => {
                let items = list.borrow().items.clone();
                entries.extend(names.iter().map(|name| &**name).zip(items));
            }
            _ => entries.extend(names.first().map(|name| (&**name, value.clone()))),
        }
    }
    match entries.as_slice() {
        [] => None,
        [(_, value)] => Some(json(value, staged)),
        _ => Some(Json::Object(
            (entries.iter())
                .map(|(name, value)| ((*name).to_owned(), json(value, staged)))
                .collect(),
        )),
    }
}

fn json(value: &Value, staged: &[(PathValue, String)]) -> Json {
    match value {
        Value::Path(path) => {
            let name = staged.iter().find(|(file, _)| file.path == path.path);
            Json::String(name.map_or_else(|| path.name().to_owned(), |(_, name)| name.clone()))
        }
        Value::List(list) => Json::Array(
            (list.borrow().items.iter())
                .map(|item| json(item, staged))
                .collect(),
        ),
        Value::Set(set) => Json::Array(
            (set.borrow().items.iter())
                .map(|item| json(&item.0, staged))
                .collect(),
        ),
        Value::Map(map) => Json::Object(
            (map.borrow().entries.iter())
                .map(|(name, value)| (name.0.to_string(), json(value, staged)))
                .collect(),
        ),
        other => crate::inputs::to_json(other),
    }
}

/// The value of a typed agent output from the model's JSON answer, checked against its
/// type. A simulated run makes up its answers, which name no real files.
pub(crate) fn bind(
    structured: &Structured,
    answer: &str,
    staged: &[(PathValue, String)],
    agent: &str,
    simulated: bool,
) -> Result<Value, Error> {
    let invalid = |reason: String| {
        failed(format!(
            "Agent `{agent}`: the answer of the model does not fit the output `{}`: {reason}",
            structured.name
        ))
    };
    let answer: Json =
        serde_json::from_str(answer.trim()).map_err(|error| invalid(error.to_string()))?;
    let answer = match &structured.kind {
        AnswerType::Record(_) => answer,
        _ => answer.get(&*structured.name).cloned().unwrap_or_default(),
    };
    let binding = Binding { staged, simulated };
    binding.value(&structured.kind, &answer).map_err(invalid)
}

/// What a path in an answer may name: a staged input by its name, or an existing file.
struct Binding<'a> {
    staged: &'a [(PathValue, String)],
    simulated: bool,
}

impl Binding<'_> {
    fn value(&self, kind: &AnswerType, answer: &Json) -> Result<Value, String> {
        let wrong = || format!("{answer} is not of the type {}", type_name(kind));
        Ok(match (kind, answer) {
            (AnswerType::Text, Json::String(text)) => Value::string(text.as_str()),
            (AnswerType::Boolean, Json::Bool(truth)) => Value::Boolean(*truth),
            (AnswerType::Integer, Json::Number(number)) => {
                let number = number.as_i64().ok_or_else(wrong)?;
                Value::Number(match i32::try_from(number) {
                    Ok(small) => Number::Integer(small),
                    Err(_) => Number::Long(number),
                })
            }
            (AnswerType::Float, Json::Number(number)) => {
                Value::Number(Number::Double(number.as_f64().ok_or_else(wrong)?))
            }
            (AnswerType::Path, Json::String(text)) => Value::Path(Rc::new(self.path(text)?)),
            (AnswerType::List(element), Json::Array(items)) => Value::list(
                (items.iter())
                    .map(|item| self.value(element, item))
                    .collect::<Result<_, _>>()?,
            ),
            (AnswerType::Record(fields), Json::Object(object)) => {
                let mut entries = Vec::new();
                for field in fields {
                    let item = object.get(&*field.name).unwrap_or(&Json::Null);
                    let value = match item {
                        Json::Null if field.optional => Value::Null,
                        Json::Null => return Err(format!("the field `{}` is missing", field.name)),
                        _ => (self.value(&field.kind, item))
                            .map_err(|reason| format!("field `{}`: {reason}", field.name))?,
                    };
                    entries.push((Value::string(&*field.name), value));
                }
                let record = Value::map(entries);
                if let Value::Map(map) = &record {
                    map.borrow_mut().kind = MapKind::Record;
                }
                record
            }
            _ => return Err(wrong()),
        })
    }

    /// A staged input by its name, else an absolute path of an existing file.
    fn path(&self, text: &str) -> Result<PathValue, String> {
        if let Some((file, _)) = self.staged.iter().find(|(_, name)| name == text) {
            return Ok(file.clone());
        }
        let path = std::path::Path::new(text);
        if self.simulated || (path.is_absolute() && path.is_file()) {
            return Ok(PathValue::parse(text, "/"));
        }
        Err(format!("`{text}` names no existing file"))
    }
}

fn type_name(kind: &AnswerType) -> &'static str {
    match kind {
        AnswerType::Boolean => "Boolean",
        AnswerType::Integer => "Integer",
        AnswerType::Float => "Float",
        AnswerType::Text => "String",
        AnswerType::Path => "Path",
        AnswerType::List(_) => "List",
        AnswerType::Record(_) => "record",
    }
}

fn key(name: &str) -> nextflow_evaluator::value::Key {
    nextflow_evaluator::value::Key(Value::string(name))
}

fn failed(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("ProcessFailedException", message)),
        span: Default::default(),
    }
}
