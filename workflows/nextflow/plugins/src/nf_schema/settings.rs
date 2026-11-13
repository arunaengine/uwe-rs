//! The `validation` config scope of nf-schema: its options with defaults and warnings, and the
//! checks that nf-schema makes when a run starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::Value;

use super::{entries, get, since, version};
use crate::Context;

/// How nf-schema reports unknown params and samplesheet columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Skip,
    Debug,
    Info,
    Warn,
    Error,
}

/// The options of the `validation` scope, read the way the selected version reads them.
pub(crate) struct Settings {
    /// `maxErrValSize`: values in error lines longer than this are cut; -1 keeps them whole.
    pub size: i64,
    /// `ignoreParams`, which always holds `nf_test_output`, and `defaultIgnoreParams`.
    pub ignored: Vec<String>,
    pub params_level: Level,
    pub headers_level: Level,
    /// `allowParamsSubstitution`, from 3.0.0 on.
    pub substitution: bool,
    pub help: Help,
    pub summary: Summary,
    /// The warnings for options of a wrong type, which a run prints when it starts.
    pub warnings: Vec<String>,
}

/// The `validation.help` options.
pub(crate) struct Help {
    /// Help through the config, which nf-schema 2 has.
    pub enabled: bool,
    pub short: String,
    pub full: String,
    /// The name of the param that shows hidden params.
    pub hidden: String,
    /// `help.showHidden`, unless the show-hidden param is a boolean.
    pub show_hidden: bool,
    /// The show-hidden param, when it is a boolean, which wins over options of `paramsHelp`.
    pub param_hidden: Option<bool>,
    pub before: String,
    pub after: String,
    pub command: String,
    /// Enum lists in help longer than this are cut; a negative value keeps them whole.
    pub enum_length: i64,
    /// The width that help descriptions wrap at: `COLUMNS`, else 100.
    pub width: usize,
}

/// The `validation.summary` options.
pub(crate) struct Summary {
    pub before: String,
    pub after: String,
    pub hidden: Vec<String>,
    pub mask: String,
    /// Java regular expressions whose matches in summary values become `mask`.
    pub patterns: Vec<String>,
}

impl Settings {
    pub(crate) fn read(context: &Context) -> Settings {
        let mut warnings = Vec::new();
        let width = (context.host.environment("COLUMNS"))
            .and_then(|columns| columns.trim().parse::<usize>().ok())
            .unwrap_or(100);
        let size = match option(context, &["maxErrValSize"]).as_ref().map(whole) {
            None => 150,
            Some(Some(size)) if size >= 1 || size == -1 => size,
            Some(_) => {
                warnings.push(SIZE_WARNING.to_owned());
                150
            }
        };
        let mut ignored = names(context, &["ignoreParams"]);
        if !ignored.iter().any(|name| name == "nf_test_output") {
            ignored.push("nf_test_output".to_owned());
        }
        ignored.extend(names(context, &["defaultIgnoreParams"]));
        for name in ["lenientMode", "monochromeLogs"] {
            flag(context, &[name], false, &mut warnings);
        }
        let substitution = since(context, [3, 0, 0])
            && match option(context, &["allowParamsSubstitution"]) {
                None => false,
                Some(Value::Boolean(value)) => value,
                Some(_) => {
                    warnings.push(
                        "Incorrect value detected for `validation.allowParamsSubstitution`, a \
                         boolean value is expected. Defaulting to `false`"
                            .to_owned(),
                    );
                    false
                }
            };
        let help = help(context, width, &mut warnings);
        let summary = Summary {
            before: text(context, &["summary", "beforeText"]),
            after: text(context, &["summary", "afterText"]),
            hidden: names(context, &["summary", "hideParams"]),
            mask: option(context, &["summary", "mask"])
                .map_or("[** masked **]".to_owned(), |mask| mask.to_string()),
            patterns: names(context, &["summary", "maskSubpaths"]),
        };
        Settings {
            size,
            ignored,
            params_level: level(context, "unrecognisedParams", &mut warnings),
            headers_level: level(context, "unrecognisedHeaders", &mut warnings),
            substitution,
            help,
            summary,
            warnings,
        }
    }
}

const SIZE_WARNING: &str =
    "`validation.maxErrValSize` needs to be a value above 0 or equal to -1. Defaulting to 150";

fn help(context: &Context, width: usize, warnings: &mut Vec<String>) -> Help {
    let name = |key: &str, default: &str| {
        option(context, &["help", key]).map_or(default.to_owned(), |value| value.to_string())
    };
    let hidden = name("showHiddenParameter", "showHidden");
    if option(context, &["showHiddenParams"]).is_some() {
        warnings.push(
            "configuration option `validation.showHiddenParams` is deprecated, please use \
             `validation.help.showHidden` or the `--showHidden` parameter instead"
                .to_owned(),
        );
    }
    let wrong = format!(
        "Incorrect value detected for `validation.help.showHidden` or `--{hidden}`, a boolean \
         is expected. Defaulting to `false`"
    );
    let configured = match option(context, &["help", "showHidden"])
        .or_else(|| option(context, &["showHiddenParams"]))
    {
        None => false,
        Some(Value::Boolean(value)) => value,
        Some(_) => {
            warnings.push(wrong.clone());
            false
        }
    };
    // 3.1.0 reads `--showHidden` from the command line, which is the text `true`, as a boolean.
    let param_hidden = match get(&context.params, &hidden) {
        None | Some(Value::Null) => None,
        Some(Value::Boolean(value)) => Some(value),
        Some(text) if since(context, [3, 1, 0]) && is_boolean(&text.to_string()) => {
            Some(text.to_string().eq_ignore_ascii_case("true"))
        }
        Some(_) => {
            warnings.push(wrong);
            Some(false)
        }
    };
    let enum_length = (option(context, &["help", "enumLength"]).as_ref())
        .and_then(whole)
        .unwrap_or(width as i64);
    Help {
        enabled: !since(context, [3, 0, 0]) && flag(context, &["help", "enabled"], false, warnings),
        short: name("shortParameter", "help"),
        full: name("fullParameter", "helpFull"),
        show_hidden: param_hidden.unwrap_or(configured),
        param_hidden,
        hidden,
        before: text(context, &["help", "beforeText"]),
        after: text(context, &["help", "afterText"]),
        command: text(context, &["help", "command"]),
        enum_length,
        width,
    }
}

/// A whole number, as Groovy's `Integer` and `Long` hold it.
fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Number(Number::Integer(value)) => Some(i64::from(*value)),
        Value::Number(Number::Long(value)) => Some(*value),
        _ => None,
    }
}

fn is_boolean(text: &str) -> bool {
    text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false")
}

/// A set option of the `validation` scope.
fn option(context: &Context, path: &[&str]) -> Option<Value> {
    let mut full = vec!["validation"];
    full.extend_from_slice(path);
    context
        .config
        .get(&full)
        .filter(|value| !matches!(value, Value::Null))
}

fn text(context: &Context, path: &[&str]) -> String {
    option(context, path).map_or(String::new(), |value| value.to_string())
}

fn names(context: &Context, path: &[&str]) -> Vec<String> {
    match option(context, path) {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        _ => Vec::new(),
    }
}

fn flag(context: &Context, path: &[&str], default: bool, warnings: &mut Vec<String>) -> bool {
    match option(context, path) {
        None => default,
        Some(Value::Boolean(value)) => value,
        Some(_) => {
            warnings.push(format!(
                "Incorrect value detected for `validation.{}`, a boolean is expected. \
                 Defaulting to `{default}`",
                path.join(".")
            ));
            default
        }
    }
}

fn level(context: &Context, name: &str, warnings: &mut Vec<String>) -> Level {
    let Some(value) = option(context, &["logging", name]) else {
        return Level::Warn;
    };
    match value.to_string().as_str() {
        "skip" => Level::Skip,
        "debug" => Level::Debug,
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => {
            warnings.push(format!(
                "Incorrect value detected for `validation.{name}`, one of (skip, debug, info, \
                 warn, error) is expected. Defaulting to `warn`"
            ));
            Level::Warn
        }
    }
}

/// Reports a message at a level: printed as Nextflow's console shows it, or as the error of
/// the call at `error`.
pub(crate) fn report(context: &mut Context, level: Level, message: &str) -> Result<(), Error> {
    match level {
        Level::Skip => {}
        Level::Debug => tracing::debug!(message, "nf-schema"),
        Level::Info => context.host.print(&format!("{message}\n")),
        Level::Warn => context.host.print(&format!("WARN: {message}")),
        Level::Error => return Err(super::thrown(message.to_owned())),
    }
    Ok(())
}

/// The options of the `validation` scope in the selected version.
fn known(context: &Context) -> Vec<&'static str> {
    let mut known = vec![
        "parametersSchema",
        "lenientMode",
        "monochromeLogs",
        "showHiddenParams",
        "maxErrValSize",
        "ignoreParams",
        "defaultIgnoreParams",
        "help.shortParameter",
        "help.fullParameter",
        "help.showHiddenParameter",
        "help.showHidden",
        "help.beforeText",
        "help.afterText",
        "help.command",
        "help.enumLength",
        "summary.beforeText",
        "summary.afterText",
        "summary.hideParams",
        "summary.mask",
        "summary.maskSubpaths",
        "logging.unrecognisedParams",
        "logging.unrecognisedHeaders",
    ];
    match since(context, [3, 0, 0]) {
        true => known.push("allowParamsSubstitution"),
        false => known.push("help.enabled"),
    }
    known
}

/// The set options of a scope as dotted names, like `help.enabled`.
fn set_options(scope: &Value, prefix: &str, found: &mut Vec<String>) {
    for (name, value) in entries(scope) {
        match value {
            Value::Map(_) => set_options(&value, &format!("{prefix}{name}."), found),
            _ => found.push(format!("{prefix}{name}")),
        }
    }
}

/// What nf-schema and Nextflow do with its config when a run starts: Nextflow warns for
/// options the plugin does not know, 2.7.3 and 2.8.0 fail on the removed `failUnrecognised*`
/// options, and nf-schema 2 prints help instead of running when the config asks for it.
pub(crate) fn start(context: &mut Context) -> Result<(), Error> {
    // The option list was recorded for 2.7.3 and later.
    let recorded = version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 3].as_slice());
    if recorded && let Some(scope) = context.config.get(&["validation"]) {
        let known = known(context);
        let mut set = Vec::new();
        set_options(&scope, "", &mut set);
        for name in set.iter().filter(|name| !known.contains(&name.as_str())) {
            let warning = format!("WARN: Unrecognized config option 'validation.{name}'\n");
            context.host.print(&warning);
        }
        if !since(context, [3, 0, 0]) {
            for name in ["failUnrecognisedParams", "failUnrecognisedHeaders"] {
                if set.iter().any(|option| option == name) {
                    let message = format!("No such variable: {name}");
                    let exception = Exception::new("MissingPropertyException", message);
                    return Err(Error::Thrown {
                        exception: Rc::new(exception),
                        span: Default::default(),
                    });
                }
            }
        }
    }
    if unpinned(context) {
        context.host.print(&pin_warning());
    }
    let settings = Settings::read(context);
    for warning in &settings.warnings {
        context.host.print(&format!("WARN: {warning}\n"));
    }
    if settings.help.enabled {
        super::help::on_start(&settings, context)?;
    }
    Ok(())
}

/// Whether the `plugins` block names nf-schema without a version.
fn unpinned(context: &Context) -> bool {
    match context.config.get(&["plugins"]) {
        Some(Value::Set(plugins)) => {
            (plugins.borrow().items.iter()).any(|item| item.0.to_string().trim() == "nf-schema")
        }
        _ => false,
    }
}

/// The box that nf-schema logs for an unpinned plugin.
fn pin_warning() -> String {
    let lines = [
        "WARNING!",
        "",
        "You just entered the danger zone!",
        "Please pin the nf-schema version in your config!",
        "Not pinning your version can't guarantee the reproducibility",
        "and the functionality of this pipeline in the future",
        "",
        "plugins {",
        "    id \"nf-schema@3.0.0\"",
        "}",
    ];
    let border = "!".repeat(67);
    let mut text = format!("WARN: {border}\n");
    for line in lines {
        text.push_str(&format!("!{line:^65}!\n"));
    }
    text.push_str(&border);
    text.push('\n');
    text
}
