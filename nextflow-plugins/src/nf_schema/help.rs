//! `paramsHelp`, and the help that nf-schema 2 prints instead of running when the config asks
//! for it: the params of a parameters schema by group, or the details of one param.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::params::parameters_schema;
use super::settings::Settings;
use super::{Schema, entries, get, since, thrown};
use crate::Context;

/// `paramsHelp`: the help as text. `parameter` is the param of 2.x's second argument.
pub(super) fn text(
    options: Option<&Value>,
    parameter: Option<String>,
    context: &mut Context,
) -> Result<Value, Error> {
    let settings = Settings::read(context);
    let option = |name: &str| options.and_then(|options| get(options, name));
    let given =
        |name: &str, default: &str| option(name).map_or(default.to_owned(), |v| v.to_string());
    let before = given("beforeText", &settings.help.before);
    let after = given("afterText", &settings.help.after);
    let command = given("command", &settings.help.command);
    let asked = match option("showHidden") {
        Some(Value::Boolean(show)) => show,
        _ => settings.help.show_hidden,
    };
    let show = settings.help.param_hidden.unwrap_or(asked);
    let full = option("fullHelp").is_some_and(|full| full.truth());
    let modern = since(context, [3, 0, 0]);
    let parameter = match modern {
        true => option("parameter").map(|parameter| parameter.to_string()),
        false => parameter,
    };
    remember(options, context);
    let schema = parameters_schema(options, context)?;
    let (body, hidden) = match parameter {
        Some(name) => (detail(&schema, &name, show, &settings, context)?, 0),
        None => listing(&schema.tree, &settings, full, show, false),
    };
    let page = Page {
        before: &before,
        command: &command,
        body: &body,
        hidden,
        show,
        after: &after,
    };
    Ok(Value::string(page.text(&settings, modern, false)))
}

/// nf-schema 2 with `validation.help.enabled`: a set help param makes the run print help and
/// end before its workflow, as Nextflow cancels it.
pub(super) fn on_start(settings: &Settings, context: &mut Context) -> Result<(), Error> {
    let param = |name: &str| get(&context.params, name).filter(Value::truth);
    let (short, full) = (param(&settings.help.short), param(&settings.help.full));
    if short.is_none() && full.is_none() {
        return Ok(());
    }
    let schema = parameters_schema(None, context)?;
    let show = settings.help.show_hidden;
    let (body, hidden) = match (full, short) {
        (Some(_), _) => listing(&schema.tree, settings, true, show, true),
        (None, Some(Value::Boolean(true))) => listing(&schema.tree, settings, false, show, true),
        (None, Some(name)) if name.to_string() == "true" => {
            listing(&schema.tree, settings, false, show, true)
        }
        (None, Some(name)) => (
            detail(&schema, &name.to_string(), show, settings, context)?,
            0,
        ),
        (None, None) => return Ok(()),
    };
    let page = Page {
        before: &settings.help.before,
        command: &settings.help.command,
        body: &body,
        hidden,
        show,
        after: &settings.help.after,
    };
    context.host.print(&page.text(settings, false, true));
    context
        .host
        .print("Execution cancelled -- Finishing pending tasks before exit\n");
    Err(Error::Thrown {
        exception: Rc::new(Exception::new("AbortOperationException", "")),
        span: Default::default(),
    })
}

/// `paramsHelp` writes the schema and help texts it used into the `validation` config.
fn remember(options: Option<&Value>, context: &Context) {
    let Some(options) = options else {
        return;
    };
    let tree = &context.config.tree;
    let set = |path: &[&str], value: Value| {
        let path: Vec<Rc<str>> = path.iter().map(|part| Rc::from(*part)).collect();
        let _ = nextflow_config::tree::assign(tree, &path, value);
    };
    if let Some(schema) = get(options, "parameters_schema") {
        set(&["validation", "parametersSchema"], schema);
    }
    for (option, key) in [
        ("beforeText", "beforeText"),
        ("afterText", "afterText"),
        ("command", "command"),
    ] {
        if let Some(value) = get(options, option) {
            set(&["validation", "help", key], value);
        }
    }
}

/// The parts of a help page around its body.
struct Page<'a> {
    before: &'a str,
    command: &'a str,
    body: &'a str,
    /// How many params the body leaves out as hidden.
    hidden: usize,
    show: bool,
    after: &'a str,
}

impl Page<'_> {
    /// The page as `paramsHelp` returns it (3.0.0 adds newlines around it), or as the start of
    /// a run prints it, with a blank line before the end.
    fn text(&self, settings: &Settings, modern: bool, start: bool) -> String {
        let mut text = self.before.to_owned();
        if modern && !start {
            text.push('\n');
        }
        if !self.command.is_empty() {
            text.push_str(&format!(
                "Typical pipeline command:\n\n  {}\n\n",
                self.command
            ));
        }
        text.push_str(self.body);
        if start || modern && !text.ends_with('\n') {
            text.push('\n');
        }
        if self.hidden > 0 && !self.show {
            text.push_str(&format!(
                " !! Hiding {} param(s), use the `--{}` parameter to show them !!\n",
                self.hidden, settings.help.hidden
            ));
        }
        text.push_str(&"-".repeat(54));
        text.push('\n');
        text.push_str(self.after);
        if modern && !start {
            text.push('\n');
        }
        text
    }
}

/// One param line before padding: its name, type and description part.
struct Line {
    name: String,
    kind: String,
    text: String,
}

/// The params by group, with the count of hidden params left out. The params outside the
/// groups and the three help params come first, without a title.
fn listing(
    tree: &Value,
    settings: &Settings,
    full: bool,
    show: bool,
    start: bool,
) -> (String, usize) {
    let mut hidden = 0;
    let mut groups = Vec::new();
    let definitions = get(tree, "$defs").or_else(|| get(tree, "definitions"));
    for (key, group) in definitions.map_or_else(Vec::new, |d| entries(&d)) {
        let title = get(&group, "title").map_or(key, |title| title.to_string());
        let properties = get(&group, "properties").unwrap_or(Value::Null);
        let lines = lines(&properties, "", full, show, &mut hidden, settings);
        groups.push((title, lines));
    }
    let properties = get(tree, "properties").unwrap_or(Value::Null);
    let mut other = lines(&properties, "", full, show, &mut hidden, settings);
    other.extend(help_lines(settings));
    let width = (other.iter())
        .chain(groups.iter().flat_map(|(_, lines)| lines))
        .map(|line| line.name.chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let mut out = Vec::new();
    let other_kind = kind_width(&other);
    for line in &other {
        out.extend(render("", width + 2, other_kind, line, settings.help.width));
    }
    out.push(String::new());
    for (title, lines) in groups.iter().filter(|(_, lines)| !lines.is_empty()) {
        out.push(title.clone());
        let kind = kind_width(lines);
        for line in lines {
            out.extend(render("  ", width, kind, line, settings.help.width));
        }
        out.push(String::new());
    }
    // `paramsHelp` drops the lines of its own params, but not their wrapped continuations.
    if !start {
        let help = &settings.help;
        let own = [&help.short, &help.full, &help.hidden].map(|name| format!("--{name}"));
        out.retain(|line| !own.iter().any(|name| line.starts_with(name.as_str())));
    }
    (out.join("\n"), hidden)
}

/// The lines of the params of a `properties` map; full help lists the params of object params
/// by dotted names.
fn lines(
    properties: &Value,
    prefix: &str,
    full: bool,
    show: bool,
    hidden: &mut usize,
    settings: &Settings,
) -> Vec<Line> {
    let mut found = Vec::new();
    for (name, definition) in entries(properties) {
        let name = format!("{prefix}{name}");
        let children = get(&definition, "properties");
        // A hidden object param stays, with its hidden params left out.
        if is_hidden(&definition) && children.is_none() && !show {
            *hidden += 1;
            continue;
        }
        match children {
            Some(children) if full => {
                let prefix = format!("{name}.");
                found.extend(lines(&children, &prefix, full, show, hidden, settings));
            }
            Some(children) => {
                let prefix = format!("{name}.");
                lines(&children, &prefix, true, show, hidden, settings);
                found.push(line(&name, &definition, true, settings));
            }
            None => found.push(line(&name, &definition, false, settings)),
        }
    }
    found
}

fn is_hidden(definition: &Value) -> bool {
    matches!(get(definition, "hidden"), Some(Value::Boolean(true)))
}

/// A param's line: its type in brackets and its description part, with the accepted values,
/// the default and, for an object in short help, a hint to its own help.
fn line(name: &str, definition: &Value, hint: bool, settings: &Settings) -> Line {
    let mut text = String::new();
    if let Some(description) = get(definition, "description") {
        text.push_str(&format!("{description} "));
    }
    if let Some(Value::List(values)) = get(definition, "enum") {
        let values: Vec<String> = values.borrow().items.iter().map(Value::to_string).collect();
        let values = cut_enum(&values.join(", "), settings.help.enum_length);
        text.push_str(&format!(" (accepted: {values}) "));
    }
    if let Some(default) = get(definition, "default").filter(|value| !matches!(value, Value::Null))
    {
        text.push_str(&format!("[default: {default}] "));
    }
    if hint {
        text.push_str(&format!(
            "(This parameter has sub-parameters. Use '--help {name}' to see all sub-parameters) "
        ));
    }
    Line {
        name: name.to_owned(),
        kind: kind(definition),
        text,
    }
}

/// The type of a param as help shows it: `[string]`, or a list of types as its text.
fn kind(definition: &Value) -> String {
    match get(definition, "type") {
        Some(Value::List(kinds)) => Value::List(kinds).to_string(),
        Some(kind) => format!("[{kind}]"),
        None => String::new(),
    }
}

/// Enum values cut to `length` characters, back to the last comma.
fn cut_enum(values: &str, length: i64) -> String {
    let characters: Vec<char> = values.chars().collect();
    let Ok(length) = usize::try_from(length) else {
        return values.to_owned();
    };
    if characters.len() <= length {
        return values.to_owned();
    }
    let kept: String = characters[..length].iter().collect();
    match kept.rfind(',') {
        Some(comma) => format!("{}, ...", &kept[..comma]),
        None => "...".to_owned(),
    }
}

fn kind_width(lines: &[Line]) -> usize {
    lines
        .iter()
        .map(|line| line.kind.chars().count())
        .max()
        .unwrap_or(0)
        + 1
}

/// A param line padded to the widths, with a long description part wrapped onto lines of
/// their own.
fn render(indent: &str, name: usize, kind: usize, line: &Line, width: usize) -> Vec<String> {
    let mut parts = wrap(&line.text, width).into_iter();
    let first = parts.next().unwrap_or_default();
    let head = format!("{indent}--{:<name$}{:<kind$}{first}", line.name, line.kind);
    std::iter::once(head).chain(parts).collect()
}

/// Text longer than `width` refilled from its words, each followed by a space, as nf-schema
/// splits it on single spaces like Java, which drops empty words at the end.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.chars().count() <= width {
        return vec![text.to_owned()];
    }
    let mut words: Vec<&str> = text.split(' ').collect();
    while words.last().is_some_and(|word| word.is_empty()) {
        words.pop();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in words {
        let length = current.chars().count() + word.chars().count() + 1;
        if length > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        current.push_str(word);
        current.push(' ');
    }
    lines.push(current);
    lines
}

/// The lines of the three help params, as nf-schema adds them to the params outside groups.
fn help_lines(settings: &Settings) -> Vec<Line> {
    let help = &settings.help;
    let (short, full) = (&help.short, &help.full);
    vec![
        Line {
            name: short.clone(),
            kind: "[boolean, string]".to_owned(),
            text: format!(
                "Show the help message for all top level parameters. When a parameter is given \
                 to `--{short}`, the full help message of that parameter will be printed. "
            ),
        },
        Line {
            name: full.clone(),
            kind: "[boolean]".to_owned(),
            text: "Show the help message for all non-hidden parameters. ".to_owned(),
        },
        Line {
            name: help.hidden.clone(),
            kind: "[boolean]".to_owned(),
            text: format!(
                "Show all hidden parameters in the help message. This needs to be used in \
                 combination with `--{short}` or `--{full}`. "
            ),
        },
    ]
}

/// The help of one param: each key of its schema, and its params for an object.
fn detail(
    schema: &Schema,
    name: &str,
    show: bool,
    settings: &Settings,
    context: &mut Context,
) -> Result<String, Error> {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    let top = find(&schema.tree, first);
    let mut definition = top.clone();
    for part in parts {
        definition = (definition.and_then(|definition| get(&definition, "properties")))
            .and_then(|properties| get(&properties, part));
    }
    let Some(definition) = definition else {
        return Err(thrown(format!(
            "Unable to create help message: Specified param '{name}' does not exist in JSON \
             schema."
        )));
    };
    let keys: Vec<(String, Value)> = entries(&definition)
        .into_iter()
        .filter(|(key, value)| {
            key != "fa_icon" && !(key == "type" && value.to_string() == "object")
        })
        .collect();
    let width = (keys.iter())
        .map(|(key, _)| match key.as_str() {
            "properties" => "options".len(),
            key => key.chars().count(),
        })
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (key, value) in keys {
        if key == "properties" {
            let mut hidden = 0;
            let prefix = format!("{name}.");
            let params = self::lines(&value, &prefix, true, show, &mut hidden, settings);
            let name_width = (params.iter())
                .map(|line| line.name.chars().count())
                .max()
                .unwrap_or(0)
                + 1;
            let kind = kind_width(&params);
            let mut options = format!("    {:<width$}: \n", "options");
            for param in &params {
                for line in render("      ", name_width, kind, param, settings.help.width) {
                    options.push_str(&format!("{line}\n"));
                }
            }
            lines.push(options);
            continue;
        }
        let value = wrap(&value.to_string(), settings.help.width).join("\n");
        lines.push(format!("    {key:<width$}: {value}"));
    }
    let text = format!("--{name}\n{}", lines.join("\n"));
    Ok(text)
}

/// A param by its name in any group of the schema or outside them.
fn find(tree: &Value, name: &str) -> Option<Value> {
    let definitions = get(tree, "$defs").or_else(|| get(tree, "definitions"));
    let groups = definitions.map_or_else(Vec::new, |d| entries(&d));
    (groups.into_iter().map(|(_, group)| group))
        .chain(std::iter::once(tree.clone()))
        .find_map(|group| get(&group, "properties").and_then(|p| get(&p, name)))
}
