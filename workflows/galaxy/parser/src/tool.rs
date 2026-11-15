//! The typed model of a Galaxy tool: identity, inputs tree and templates.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value as Json;

use crate::Error;
use crate::xml::{self, Element};

/// A tool after macro expansion. Text is kept as the XML gives it; values are typed when used.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub version: String,
    pub profile: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub biotools: Option<String>,
    pub dois: Vec<String>,
    /// Whether text and select values are sanitized, from `<options sanitize>`.
    pub sanitize: bool,
    pub inputs: Vec<Input>,
    pub command: Option<Template>,
    /// From the `detect_errors` attribute of the command, like `exit_code`.
    pub detect_errors: Option<String>,
    pub configfiles: Vec<Configfile>,
}

/// A Cheetah template with the line of its element, for errors.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub text: String,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Configfile {
    pub name: String,
    pub filename: Option<String>,
    pub template: Template,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Param(Param),
    Conditional(Conditional),
    Section(Section),
    Repeat(Repeat),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub label: Option<String>,
    pub help: Option<String>,
    pub argument: Option<String>,
    pub optional: bool,
    /// A text param without an `optional` attribute whose validators accept "".
    pub optional_inferred: bool,
    /// The `value` attribute.
    pub value: Option<String>,
    pub multiple: bool,
    pub options: Vec<SelectOption>,
    /// Options that come from a data table, a dataset or code, which tier 1 cannot list.
    pub dynamic: bool,
    pub separator: String,
    pub truevalue: String,
    pub falsevalue: String,
    pub checked: Option<bool>,
    /// The Galaxy datatypes a data param takes.
    pub formats: Vec<String>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub rgb: bool,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Text,
    Integer,
    Float,
    Boolean,
    Select,
    Hidden,
    Color,
    Data,
    Collection,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conditional {
    pub name: String,
    pub test: Param,
    pub cases: Vec<Case>,
}

/// A `<when>` of a conditional.
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub value: String,
    pub inputs: Vec<Input>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    pub title: Option<String>,
    pub inputs: Vec<Input>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repeat {
    pub name: String,
    pub title: Option<String>,
    pub inputs: Vec<Input>,
}

/// Reads the tool at `path`, with the macro files beside it.
pub fn load(path: &Path) -> Result<Tool, Error> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let file = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| Error::Import {
        path: file.clone(),
        source,
    })?;
    parse(&text, &file, &mut |import| {
        let path = folder.join(import);
        std::fs::read_to_string(&path).map_err(|source| Error::Import {
            path: path.display().to_string(),
            source,
        })
    })
}

/// Reads a tool from its text; `load` reads the macro files it imports.
pub fn parse(
    text: &str,
    file: &str,
    load: &mut dyn FnMut(&str) -> Result<String, Error>,
) -> Result<Tool, Error> {
    let mut root = xml::read(text, file)?;
    crate::macros::expand(&mut root, load)?;
    if root.name != "tool" {
        return Err(Error::Document {
            file: file.to_owned(),
            line: root.line,
            message: format!("the root element is <{}>, not <tool>", root.name),
        });
    }
    tool(&root, file)
}

fn tool(root: &Element, file: &str) -> Result<Tool, Error> {
    let text = |element: Option<&Element>| element.map(|found| found.content().trim().to_owned());
    let id = root.attribute("id").unwrap_or_default().to_owned();
    let command = root.child("command");
    let mut configfiles = Vec::new();
    for configfile in root
        .child("configfiles")
        .iter()
        .flat_map(|c| c.named("configfile"))
    {
        configfiles.push(Configfile {
            name: configfile.attribute("name").unwrap_or_default().to_owned(),
            filename: configfile.attribute("filename").map(str::to_owned),
            template: template(configfile),
        });
    }
    let inputs = match root.child("inputs") {
        Some(inputs) => inputs_of(inputs, file)?,
        None => Vec::new(),
    };
    Ok(Tool {
        name: root.attribute("name").unwrap_or_default().to_owned(),
        version: root.attribute("version").unwrap_or("1.0.0").to_owned(),
        profile: root.attribute("profile").map(str::to_owned),
        license: root.attribute("license").map(str::to_owned),
        description: text(root.child("description")).filter(|found| !found.is_empty()),
        biotools: (root.child("xrefs").iter())
            .flat_map(|xrefs| xrefs.named("xref"))
            .find(|xref| xref.attribute("type") == Some("bio.tools"))
            .map(|xref| xref.content().trim().to_owned()),
        dois: (root.child("citations").iter())
            .flat_map(|citations| citations.named("citation"))
            .filter(|citation| citation.attribute("type") == Some("doi"))
            .map(|citation| citation.content().trim().to_owned())
            .collect(),
        sanitize: (root.child("options").and_then(|o| o.attribute("sanitize"))).is_none_or(truthy),
        command: command.map(template),
        detect_errors: command
            .and_then(|c| c.attribute("detect_errors"))
            .map(str::to_owned),
        configfiles,
        id,
        inputs,
    })
}

fn template(element: &Element) -> Template {
    Template {
        text: element.content(),
        line: element.line,
    }
}

fn inputs_of(parent: &Element, file: &str) -> Result<Vec<Input>, Error> {
    let mut inputs = Vec::new();
    for element in &parent.children {
        let input = match element.name.as_str() {
            "param" => Input::Param(param(element, file)?),
            "conditional" => {
                let Some(test) = element.child("param") else {
                    return Err(Error::Document {
                        file: file.to_owned(),
                        line: element.line,
                        message: "a conditional without a test param".to_owned(),
                    });
                };
                let mut cases = Vec::new();
                for case in element.named("when") {
                    cases.push(Case {
                        value: case.attribute("value").unwrap_or_default().to_owned(),
                        inputs: inputs_of(case, file)?,
                    });
                }
                Input::Conditional(Conditional {
                    name: element.attribute("name").unwrap_or_default().to_owned(),
                    test: param(test, file)?,
                    cases,
                })
            }
            "section" => Input::Section(Section {
                name: element.attribute("name").unwrap_or_default().to_owned(),
                title: element.attribute("title").map(str::to_owned),
                inputs: inputs_of(element, file)?,
            }),
            "repeat" => Input::Repeat(Repeat {
                name: element.attribute("name").unwrap_or_default().to_owned(),
                title: element.attribute("title").map(str::to_owned),
                inputs: inputs_of(element, file)?,
            }),
            _ => continue,
        };
        inputs.push(input);
    }
    Ok(inputs)
}

fn param(element: &Element, file: &str) -> Result<Param, Error> {
    let attribute = |name: &str| element.attribute(name).map(str::to_owned);
    let argument = attribute("argument");
    let name = match (attribute("name"), &argument) {
        (Some(name), _) => name,
        (None, Some(argument)) => argument.trim_start_matches('-').replace('-', "_"),
        (None, None) => {
            return Err(Error::Document {
                file: file.to_owned(),
                line: element.line,
                message: "a param without a name or argument".to_owned(),
            });
        }
    };
    let kind = match element.attribute("type").unwrap_or_default() {
        "text" => ParamKind::Text,
        "integer" => ParamKind::Integer,
        "float" => ParamKind::Float,
        "boolean" => ParamKind::Boolean,
        "select" => ParamKind::Select,
        "hidden" => ParamKind::Hidden,
        "color" => ParamKind::Color,
        "data" => ParamKind::Data,
        "data_collection" => ParamKind::Collection,
        other => ParamKind::Other(other.to_owned()),
    };
    let multiple = element.attribute("multiple").is_some_and(truthy);
    let given = element.attribute("optional");
    let (optional, optional_inferred) = match (&kind, given) {
        (_, Some(given)) => (truthy(given), false),
        (ParamKind::Text, None) => {
            let empty = accepts_empty(element);
            (empty, empty)
        }
        (ParamKind::Select, None) => (multiple, false),
        (_, None) => (false, false),
    };
    let options = element
        .named("option")
        .map(|option| SelectOption {
            value: option.attribute("value").unwrap_or_default().to_owned(),
            label: option.content().trim().to_owned(),
            selected: option.attribute("selected").is_some_and(truthy),
        })
        .collect();
    let dynamic =
        element.child("options").is_some() || element.attribute("dynamic_options").is_some();
    Ok(Param {
        kind,
        label: attribute("label"),
        help: attribute("help")
            .or_else(|| element.child("help").map(|h| h.content().trim().to_owned())),
        optional,
        optional_inferred,
        value: attribute("value"),
        multiple,
        options,
        dynamic,
        separator: attribute("separator").unwrap_or_else(|| ",".to_owned()),
        truevalue: attribute("truevalue").unwrap_or_else(|| "true".to_owned()),
        falsevalue: attribute("falsevalue").unwrap_or_else(|| "false".to_owned()),
        checked: element.attribute("checked").map(truthy),
        formats: (element.attribute("format").unwrap_or_default().split(','))
            .map(str::trim)
            .filter(|format| !format.is_empty())
            .map(str::to_owned)
            .collect(),
        min: attribute("min"),
        max: attribute("max"),
        rgb: element.attribute("rgb").is_some_and(truthy),
        line: element.line,
        argument,
        name,
    })
}

/// Whether the static validators of a text param accept "", which makes it optional.
fn accepts_empty(element: &Element) -> bool {
    element.named("validator").all(|validator| {
        let negate = validator.attribute("negate").is_some_and(truthy);
        let passes = match validator.attribute("type").unwrap_or_default() {
            "regex" => regex::Regex::new(&format!("^(?:{})", validator.content()))
                .is_ok_and(|pattern| pattern.is_match("")),
            "length" => validator
                .attribute("min")
                .and_then(|min| min.parse::<f64>().ok())
                .is_none_or(|min| min <= 0.0),
            "empty_field" | "in_range" | "expression" => false,
            _ => return true,
        };
        passes != negate
    })
}

impl Param {
    /// The raw value of the param when nothing is given, as Galaxy's `get_initial_value`;
    /// null for none. `profile` is the tool's profile.
    pub fn initial(&self, profile: Option<&str>) -> Json {
        match self.kind {
            ParamKind::Text | ParamKind::Hidden => {
                self.value.clone().map_or(Json::Null, Json::String)
            }
            ParamKind::Integer => (self.value.as_deref())
                .and_then(|value| value.trim().parse::<i64>().ok())
                .map_or(Json::Null, Json::from),
            ParamKind::Float => (self.value.as_deref())
                .and_then(|value| value.trim().parse::<f64>().ok())
                .map_or(Json::Null, Json::from),
            ParamKind::Boolean => match self.checked {
                None if self.optional && at_least(profile, "26.2") => Json::Null,
                checked => Json::Bool(checked.unwrap_or(false)),
            },
            ParamKind::Color => {
                Json::String((self.value.as_deref()).unwrap_or("#000000").to_lowercase())
            }
            ParamKind::Select if self.dynamic => Json::Null,
            ParamKind::Select => {
                let selected: Vec<Json> = (self.options.iter())
                    .filter(|option| option.selected)
                    .map(|option| Json::String(option.value.clone()))
                    .collect();
                match (selected.first(), self.multiple) {
                    (None, false) if !self.optional => (self.options.first())
                        .map_or(Json::Null, |option| Json::String(option.value.clone())),
                    (None, _) => Json::Null,
                    (Some(first), false) => first.clone(),
                    (Some(_), true) => Json::Array(selected),
                }
            }
            ParamKind::Data | ParamKind::Collection | ParamKind::Other(_) => Json::Null,
        }
    }
}

/// Whether a profile is at least `version`, comparing dotted numbers; no profile is older.
pub fn at_least(profile: Option<&str>, version: &str) -> bool {
    let numbers = |text: &str| -> Vec<u64> {
        text.split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    profile.is_some_and(|profile| numbers(profile) >= numbers(version))
}

/// Galaxy's `string_as_bool`.
pub fn truthy(text: &str) -> bool {
    matches!(text.to_lowercase().as_str(), "true" | "yes" | "on" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(inputs: &str, profile: &str) -> Vec<Param> {
        let text = format!("<tool id=\"t\" profile=\"{profile}\"><inputs>{inputs}</inputs></tool>");
        let tool = parse(&text, "t.xml", &mut |_| unreachable!("no imports")).expect("a tool");
        (tool.inputs.into_iter())
            .map(|input| match input {
                Input::Param(param) => param,
                other => panic!("a param, not {other:?}"),
            })
            .collect()
    }

    #[test]
    fn names_and_optionality() {
        let found = params(
            r#"<param argument="--count-table" type="data" format="tabular,txt"/>
            <param name="free" type="text"/>
            <param name="checked" type="text"><validator type="regex">^[\w,]+$</validator></param>
            <param name="choice" type="select" multiple="true"><option value="a"/></param>"#,
            "25.1",
        );
        assert_eq!(found[0].name, "count_table");
        assert_eq!(found[0].formats, ["tabular", "txt"]);
        assert!(
            found[1].optional && found[1].optional_inferred,
            "no validator accepts \"\""
        );
        assert!(!found[2].optional, "the regex refuses \"\"");
        assert!(found[3].optional, "a multiple select is optional");
    }

    #[test]
    fn initial_values() {
        let found = params(
            r#"<param name="t" type="text" value="x"/>
            <param name="i" type="integer" value="5"/>
            <param name="f" type="float" optional="true"/>
            <param name="b" type="boolean" checked="yes"/>
            <param name="o" type="boolean" optional="true"/>
            <param name="s" type="select"><option value="a"/><option value="b"/></param>
            <param name="m" type="select" optional="true"><option value="a"/><option value="b" selected="true"/></param>
            <param name="c" type="color"/>"#,
            "26.2",
        );
        let initial: Vec<Json> = found
            .iter()
            .map(|param| param.initial(Some("26.2")))
            .collect();
        assert_eq!(
            initial,
            [
                Json::from("x"),
                Json::from(5),
                Json::Null,
                Json::Bool(true),
                Json::Null,
                Json::from("a"),
                Json::from("b"),
                Json::from("#000000")
            ]
        );
        assert_eq!(
            found[4].initial(Some("25.1")),
            Json::Bool(false),
            "unset before 26.2"
        );
    }
}
