//! The values of a Galaxy job: the job file and the parameters of the command line, typed by
//! the params of the tool, and the files of its data inputs by their paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use galaxy_parser::definition::case_value;
use galaxy_parser::tool::{Input, Param, ParamKind, Tool};
use serde_json::{Map, Value as Json};
use workflow_graph::{DriverError, Launch};

/// A file of a data input: where it is, its Galaxy datatype and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dataset {
    pub location: String,
    pub ext: String,
    pub name: String,
}

/// The values of a launch, nested like the inputs: the job file, JSON or YAML, then the
/// parameters of the command line, with the folder that relative paths start from.
pub(crate) fn given(launch: &Launch) -> Result<(Map<String, Json>, PathBuf), DriverError> {
    let mut values = Map::new();
    let mut base = std::env::current_dir().unwrap_or_default();
    if let Some(path) = &launch.inputs {
        let shown = path.display();
        let text = std::fs::read_to_string(path)
            .map_err(|error| DriverError::new(format!("cannot read {shown}: {error}")))?;
        let read = match serde_json::from_str(&text) {
            Ok(json) => json,
            Err(_) => tool_definition::yaml::read(&text)
                .map_err(|error| DriverError::new(format!("{shown}: {error}")))?,
        };
        match read {
            Json::Object(object) => values = object,
            Json::Null => {}
            _ => return Err(DriverError::new(format!("{shown} must hold an object"))),
        }
        let file = std::path::absolute(path).unwrap_or(path.clone());
        base = file.parent().unwrap_or(Path::new("/")).to_owned();
    }
    for (name, text) in &launch.params {
        values.insert(name.clone(), Json::String(text.clone()));
    }
    Ok((nested(values), base))
}

/// Values with the names `a.b` and `a|b`, as wir ports and the command line name the inputs
/// inside sections and conditionals, nested as `{"a": {"b": ...}}`.
pub(crate) fn nested(values: Map<String, Json>) -> Map<String, Json> {
    let mut nested = Map::new();
    for (name, value) in values {
        let parts: Vec<&str> = name.split(['.', '|']).collect();
        let (last, path) = parts.split_last().unwrap_or((&"", &[]));
        let mut object = &mut nested;
        for part in path {
            let entry = object
                .entry((*part).to_owned())
                .or_insert_with(|| Json::Object(Map::new()));
            if !entry.is_object() {
                *entry = Json::Object(Map::new());
            }
            let Json::Object(inner) = entry else {
                unreachable!("made an object above")
            };
            object = inner;
        }
        merge(object, last, plain(value));
    }
    nested
}

/// Sets `key` to `value`, keeping the entries of an object that `key` holds already, as
/// `adv.mask` and `adv` may both give parts of the section `adv`.
fn merge(object: &mut Map<String, Json>, key: &str, value: Json) {
    match (object.get_mut(key), value) {
        (Some(Json::Object(known)), Json::Object(given)) => {
            for (inner, item) in given {
                merge(known, &inner, item);
            }
        }
        (_, value) => {
            object.insert(key.to_owned(), value);
        }
    }
}

/// A File or Directory object as its location, a path without `file://`; arrays and objects
/// keep their shape.
pub(crate) fn plain(value: Json) -> Json {
    match value {
        Json::Object(object) if object.contains_key("class") => {
            let location = (object.get("path").or_else(|| object.get("location")))
                .and_then(Json::as_str)
                .unwrap_or_default();
            Json::String(local(location).to_owned())
        }
        Json::Object(object) => {
            Json::Object(object.into_iter().map(|(k, v)| (k, plain(v))).collect())
        }
        Json::Array(items) => Json::Array(items.into_iter().map(plain).collect()),
        other => other,
    }
}

/// A location without its `file://` scheme.
pub(crate) fn local(location: &str) -> &str {
    location.strip_prefix("file://").unwrap_or(location)
}

/// The parameters typed by the params of `inputs`: integers, floats and booleans that come as
/// text, and multiple selects as lists. Keys of Galaxy's own, like `__current_case__`, go.
pub(crate) fn typed(inputs: &[Input], values: &Map<String, Json>) -> Map<String, Json> {
    let mut typed = Map::new();
    for (name, value) in values {
        if name.starts_with("__") || value.get("__class__").is_some() {
            continue;
        }
        let value = match declared(inputs, name) {
            Some(Input::Param(param)) => typed_param(param, value),
            Some(Input::Section(section)) => object(&section.inputs, value),
            Some(Input::Conditional(conditional)) => {
                let inner: Vec<Input> = std::iter::once(Input::Param(conditional.test.clone()))
                    .chain(
                        conditional
                            .cases
                            .iter()
                            .flat_map(|case| case.inputs.clone()),
                    )
                    .collect();
                object(&inner, value)
            }
            Some(Input::Repeat(repeat)) => match value {
                Json::Array(items) => Json::Array(
                    (items.iter())
                        .map(|item| object(&repeat.inputs, item))
                        .collect(),
                ),
                other => other.clone(),
            },
            None => value.clone(),
        };
        typed.insert(name.clone(), value);
    }
    typed
}

fn object(inputs: &[Input], value: &Json) -> Json {
    match value {
        Json::Object(entries) => Json::Object(typed(inputs, entries)),
        other => other.clone(),
    }
}

fn declared<'a>(inputs: &'a [Input], name: &str) -> Option<&'a Input> {
    inputs.iter().find(|input| match input {
        Input::Param(param) => param.name == name,
        Input::Section(section) => section.name == name,
        Input::Conditional(conditional) => conditional.name == name,
        Input::Repeat(repeat) => repeat.name == name,
    })
}

fn typed_param(param: &Param, value: &Json) -> Json {
    let Json::String(text) = value else {
        return value.clone();
    };
    let trimmed = text.trim();
    match param.kind {
        ParamKind::Integer => trimmed.parse::<i64>().map_or(value.clone(), Json::from),
        ParamKind::Float => trimmed.parse::<f64>().map_or(value.clone(), Json::from),
        ParamKind::Boolean if *text == param.truevalue => Json::Bool(true),
        ParamKind::Boolean if *text == param.falsevalue => Json::Bool(false),
        ParamKind::Boolean => Json::Bool(galaxy_parser::tool::truthy(trimmed)),
        ParamKind::Select if param.multiple => Json::Array(
            (text.split(','))
                .map(|part| Json::String(part.trim().to_owned()))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// The files of the data params by path, like `adv|control`, and the other values. A data
/// value is a path or a list of them; relative paths start from `base`.
pub(crate) fn split(
    tool: &Tool,
    values: &Map<String, Json>,
    base: &Path,
) -> (Map<String, Json>, BTreeMap<String, Vec<Dataset>>) {
    let mut datasets = BTreeMap::new();
    let parameters = walk(tool, &tool.inputs, values, "", base, &mut datasets);
    (parameters, datasets)
}

fn walk(
    tool: &Tool,
    inputs: &[Input],
    values: &Map<String, Json>,
    prefix: &str,
    base: &Path,
    datasets: &mut BTreeMap<String, Vec<Dataset>>,
) -> Map<String, Json> {
    let mut kept = values.clone();
    for input in inputs {
        match input {
            Input::Param(param)
                if matches!(param.kind, ParamKind::Data | ParamKind::Collection) =>
            {
                let Some(value) = kept.remove(&param.name) else {
                    continue;
                };
                let files = files(param, &value, base);
                if !files.is_empty() {
                    datasets.insert(format!("{prefix}{}", param.name), files);
                }
            }
            Input::Param(_) => {}
            Input::Section(section) => {
                if let Some(Json::Object(inner)) = kept.get(&section.name) {
                    let path = format!("{prefix}{}|", section.name);
                    let inner = walk(tool, &section.inputs, inner, &path, base, datasets);
                    kept.insert(section.name.clone(), Json::Object(inner));
                }
            }
            Input::Conditional(conditional) => {
                let Some(Json::Object(inner)) = kept.get(&conditional.name) else {
                    continue;
                };
                let test = &conditional.test;
                let chosen = (inner.get(&test.name).cloned())
                    .unwrap_or_else(|| test.initial(tool.profile.as_deref()));
                let case = case_value(test, &chosen);
                let Some(case) = (conditional.cases.iter())
                    .find(|found| Some(found.value.as_str()) == case.as_deref())
                else {
                    continue;
                };
                let path = format!("{prefix}{}|", conditional.name);
                let inner = walk(tool, &case.inputs, inner, &path, base, datasets);
                kept.insert(conditional.name.clone(), Json::Object(inner));
            }
            Input::Repeat(repeat) => {
                let Some(Json::Array(items)) = kept.get(&repeat.name) else {
                    continue;
                };
                let mut walked = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    let path = format!("{prefix}{}_{index}|", repeat.name);
                    walked.push(match item {
                        Json::Object(inner) => {
                            Json::Object(walk(tool, &repeat.inputs, inner, &path, base, datasets))
                        }
                        other => other.clone(),
                    });
                }
                kept.insert(repeat.name.clone(), Json::Array(walked));
            }
        }
    }
    kept
}

/// The files of a data value, with the datatype that the name gives where the param takes
/// it, else the first datatype the param takes.
fn files(param: &Param, value: &Json, base: &Path) -> Vec<Dataset> {
    let items = match value {
        Json::Array(items) => items.clone(),
        Json::Null => Vec::new(),
        other => vec![other.clone()],
    };
    (items.iter())
        .filter_map(|item| match plain(item.clone()) {
            Json::String(location) if !location.is_empty() => Some(location),
            _ => None,
        })
        .map(|location| {
            let location = match location.contains("://") || Path::new(&location).is_absolute() {
                true => location,
                false => base.join(&location).to_string_lossy().into_owned(),
            };
            let name = location.rsplit('/').next().unwrap_or_default().to_owned();
            let ending = name
                .split_once('.')
                .map(|(_, ending)| ending.to_ascii_lowercase());
            let ext = match ending {
                Some(ending) if param.formats.contains(&ending) => ending,
                _ => (param.formats.first().cloned()).unwrap_or_else(|| "data".to_owned()),
            };
            Dataset {
                location,
                ext,
                name,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TOOL: &str = r#"<tool id="t"><inputs>
        <param name="reads" type="data" format="fastqsanger,fasta"/>
        <param name="count" type="integer" value="1"/>
        <section name="adv"><param name="ratio" type="float" value="0.5"/>
            <param name="mask" type="data" format="bed" optional="true"/></section>
        <conditional name="mode"><param name="layout" type="select">
            <option value="single"/><option value="paired"/></param>
            <when value="single"/><when value="paired">
            <param name="mates" type="data" format="fastqsanger"/></when></conditional>
        </inputs></tool>"#;

    fn tool() -> Tool {
        galaxy_parser::tool::parse(TOOL, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool")
    }

    #[test]
    fn nests_dotted_names() {
        let values = Map::from_iter([
            ("adv.ratio".to_owned(), json!("0.25")),
            (
                "adv|mask".to_owned(),
                json!({"class": "File", "location": "file:///m.bed"}),
            ),
            ("count".to_owned(), json!("3")),
        ]);
        let nested = nested(values);
        assert_eq!(
            Json::Object(nested.clone()),
            json!({"adv": {"ratio": "0.25", "mask": "/m.bed"}, "count": "3"})
        );
        let typed = typed(&tool().inputs, &nested);
        assert_eq!(typed["count"], json!(3));
        assert_eq!(typed["adv"]["ratio"], json!(0.25));
    }

    #[test]
    fn merges_sections() {
        let values = Map::from_iter([
            ("adv.mask".to_owned(), json!("m.bed")),
            ("adv".to_owned(), json!({"ratio": 0.1})),
        ]);
        assert_eq!(
            Json::Object(nested(values)),
            json!({"adv": {"mask": "m.bed", "ratio": 0.1}})
        );
    }

    #[test]
    fn splits_datasets() {
        let tool = tool();
        let values = json!({"reads": "in/a.fasta", "adv": {"mask": "/data/m.bed"},
            "mode": {"layout": "paired", "mates": "b.fq"}});
        let Json::Object(values) = values else {
            unreachable!()
        };
        let (parameters, datasets) = split(&tool, &values, Path::new("/work"));
        assert_eq!(
            Json::Object(parameters),
            json!({"adv": {}, "mode": {"layout": "paired"}})
        );
        let reads = &datasets["reads"][0];
        assert_eq!(
            (reads.location.as_str(), reads.ext.as_str()),
            ("/work/in/a.fasta", "fasta")
        );
        assert_eq!(datasets["adv|mask"][0].location, "/data/m.bed");
        assert_eq!(
            datasets["mode|mates"][0].ext, "fastqsanger",
            "the first datatype"
        );
    }
}
