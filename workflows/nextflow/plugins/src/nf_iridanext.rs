//! nf-iridanext 0.3.0: writes the IRIDA Next JSON of published files and sample metadata at the
//! end of a successful run; `loadIridaSampleIds` registers sample ids.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use nextflow_config::Config;
use nextflow_evaluator::csv::{CsvOptions, records};
use nextflow_evaluator::interpreter::glob::matches;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::entries;
use crate::observer::{Event, Observer, Part, Task};
use crate::report_format::{Json, ordered_json};
use crate::service::safe_url;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["loadIridaSampleIds"],
    services: &["loadIridaSampleIds"],
    call: Some(call),
    script: Some(SCRIPT),
    scopes: &["iridanext"],
    observer: Some(activate),
    ..Plugin::new("nf-iridanext", Kind::Observer, State::Implemented)
};

/// The operator, which passes each item on after registering the id of its meta map.
const SCRIPT: &str = "include { registerSampleId } from 'plugin/nf-iridanext'\n\
    def loadIridaSampleIds(source) {\n    \
    return source.map { item -> registerSampleId(item); item }\n}\n";

/// The schema that `output.validate` checks against when `output.schema` names none.
const SCHEMA: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "files": {
      "type": "object",
      "properties": {
        "global": { "type": "array" },
        "samples": { "type": "object" }
      }
    },
    "metadata": {
      "type": "object",
      "properties": { "samples": { "type": "object" } }
    }
  }
}"#;

thread_local! {
    /// The sample ids that `loadIridaSampleIds` registered in the run of this thread.
    static REGISTERED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match name {
        "registerSampleId" => {
            let key = text(context.config, &["files", "idkey"]).unwrap_or("id".into());
            register(arguments.first().unwrap_or(&Value::Null), &key);
            Ok(Value::Null)
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-iridanext`"),
            span: Default::default(),
        }),
    }
}

/// Registers the id of the meta map that is the first element of an item, or warns.
fn register(item: &Value, key: &str) {
    let first = match item {
        Value::List(list) => list.borrow().items.first().cloned(),
        _ => None,
    };
    match first {
        Some(Value::Map(map)) => {
            let found = (map.borrow().entries.iter())
                .find(|(name, _)| name.0.to_string() == key)
                .map(|(_, value)| value.to_string());
            match found {
                Some(id) => REGISTERED.with(|ids| ids.borrow_mut().push(id)),
                None => {
                    let meta = Value::Map(map.clone()).to_string();
                    tracing::warn!(
                        "The expected key ({key}) was not found in the meta map ({meta})."
                    );
                }
            }
        }
        other => {
            let found = other.unwrap_or(Value::Null).to_string();
            tracing::warn!("Expected a Map object in channel, but found {found}.");
        }
    }
}

/// A text setting of the `iridanext.output` scope.
fn text(config: &Config, path: &[&str]) -> Option<String> {
    let full: Vec<&str> = ["iridanext", "output"]
        .iter()
        .chain(path)
        .copied()
        .collect();
    config
        .get(&full)
        .filter(|value| !matches!(value, Value::Null))
        .map(|value| value.to_string())
}

fn flag(config: &Config, path: &[&str], default: bool) -> bool {
    let full: Vec<&str> = ["iridanext", "output"]
        .iter()
        .chain(path)
        .copied()
        .collect();
    match config.get(&full) {
        None | Some(Value::Null) => default,
        Some(value) => value.truth(),
    }
}

/// The texts of a setting that is one text or a list of them.
fn texts(value: &Value) -> Vec<String> {
    match value {
        Value::List(list) => list.borrow().items.iter().map(Value::to_string).collect(),
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    }
}

/// Where a metadata parser reads its samples from.
#[derive(Debug)]
enum Parser {
    Csv {
        glob: Option<String>,
        column: String,
        separator: char,
    },
    Json {
        glob: Option<String>,
    },
}

/// The settings of the `iridanext` scope.
#[derive(Debug)]
struct Settings {
    path: Option<String>,
    overwrite: bool,
    relativize: bool,
    validate: bool,
    schema: Option<String>,
    key: String,
    global: Vec<String>,
    samples: Vec<String>,
    parsers: Vec<Parser>,
    flatten: bool,
    ignore: Vec<String>,
    keep: Option<Vec<String>>,
    rename: Vec<(String, String)>,
}

fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    REGISTERED.with(|ids| ids.borrow_mut().clear());
    let settings = settings(config)?;
    Ok(Some(Box::new(Irida {
        settings,
        launch: PathBuf::new(),
        tasks: Vec::new(),
        published: Vec::new(),
        failure: None,
    })))
}

fn settings(config: &Config) -> Result<Settings, String> {
    let mut globs = HashMap::new();
    let files = config.get(&["iridanext", "output", "files"]);
    for (scope, value) in entries(&files.unwrap_or(Value::Null)) {
        if scope == "idkey" {
            continue;
        }
        if !matches!(value, Value::String(_) | Value::GString(_) | Value::List(_)) {
            return Err(format!(
                "Invalid configuration for iridanext.files={}",
                map_text(&scope, &value)
            ));
        }
        globs.insert(scope, texts(&value));
    }
    let samples = config.get(&["iridanext", "output", "metadata", "samples"]);
    let samples = samples.unwrap_or(Value::Null);
    let mut parsers = Vec::new();
    let mut flatten = false;
    let (mut ignore, mut keep, mut rename) = (Vec::new(), None, Vec::new());
    for (name, value) in entries(&samples) {
        match name.as_str() {
            "flatten" => flatten = value.truth(),
            "ignore" => ignore = texts(&value),
            "keep" => keep = Some(texts(&value)),
            "rename" => {
                rename = (entries(&value).into_iter())
                    .map(|(from, to)| (from, to.to_string()))
                    .collect()
            }
            _ => parsers.push(parser(&name, &value, &samples)?),
        }
    }
    let path = text(config, &["path"]);
    if let Some(path) = &path
        && storage::address::scheme(path).is_some()
    {
        return Err(format!(
            "nf-iridanext writes iridanext.output.path only to a local file, not {}",
            safe_url(path)
        ));
    }
    Ok(Settings {
        path,
        overwrite: flag(config, &["overwrite"], false),
        relativize: flag(config, &["relativize"], true),
        validate: flag(config, &["validate"], false),
        schema: text(config, &["schema"]),
        key: text(config, &["files", "idkey"]).unwrap_or("id".into()),
        global: globs.remove("global").unwrap_or_default(),
        samples: globs.remove("samples").unwrap_or_default(),
        parsers,
        flatten,
        ignore,
        keep,
        rename,
    })
}

/// A metadata parser entry of `output.metadata.samples`.
fn parser(name: &str, value: &Value, samples: &Value) -> Result<Parser, String> {
    if !matches!(value, Value::Map(_)) {
        return Err(format!(
            "Invalid config for iridanext.output.metadata.samples: {samples}"
        ));
    }
    let setting = |key: &str| {
        let found = entries(value).into_iter().find(|(name, _)| name == key);
        found.map(|(_, value)| value.to_string())
    };
    match name {
        "csv" => Ok(Parser::Csv {
            glob: setting("path"),
            column: setting("idcol").unwrap_or("id".into()),
            separator: setting("sep")
                .and_then(|sep| sep.chars().next())
                .unwrap_or(','),
        }),
        "json" => Ok(Parser::Json {
            glob: setting("path"),
        }),
        _ => Err(format!(
            "Invalid type={name} for iridanext.output.metadata.samples: {samples}"
        )),
    }
}

/// A setting as Groovy prints a one-entry map.
fn map_text(key: &str, value: &Value) -> String {
    format!("[{key}:{value}]")
}

/// The observer: the finished tasks and the published files, which the end of a successful run
/// turns into the output file.
struct Irida {
    settings: Settings,
    launch: PathBuf,
    tasks: Vec<Task>,
    /// Each published file, source and target, in the order they were published.
    published: Vec<(String, String)>,
    failure: Option<String>,
}

impl Observer for Irida {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.launch = PathBuf::from(&start.launch_dir),
            Event::TaskCompleted(task) | Event::TaskCached(task) => self.tasks.push(task.clone()),
            Event::FilePublished { source, target } => {
                if self.published.iter().any(|(known, _)| known == source) {
                    let message = format!("Error: file with source={source} was already published");
                    self.failure.get_or_insert(message);
                }
                self.published.push((source.clone(), target.clone()));
            }
            Event::RunEnded(end) if end.success && self.failure.is_none() => {
                if let Err(message) = self.write() {
                    tracing::warn!("{message}");
                }
            }
            _ => {}
        }
    }

    fn check(&mut self) -> Result<(), String> {
        self.failure.clone().map_or(Ok(()), Err)?;
        let sources = (self.settings.parsers.iter()).filter_map(|parser| self.source(parser));
        match sources
            .into_iter()
            .find(|target| storage::address::scheme(target).is_some())
        {
            Some(target) => Err(format!(
                "nf-iridanext reads sample metadata only from local files, not {}",
                safe_url(target)
            )),
            None => Ok(()),
        }
    }
}

impl Irida {
    /// Writes the output file, if the config names one.
    fn write(&self) -> Result<(), String> {
        let Some(path) = &self.settings.path else {
            return Ok(());
        };
        let path = self.launch.join(path);
        let report = self.report(path.parent().unwrap_or(Path::new("/")))?;
        if path.exists() && !self.settings.overwrite {
            return Err(format!(
                "Error: iridanext.output.path={} exists and iridanext.output.overwrite=false",
                path.display()
            ));
        }
        if self.settings.validate {
            self.validate(&report, &path)?;
        }
        let text = report.pretty();
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        }
        let written = match path.to_string_lossy().ends_with(".gz") {
            true => std::fs::File::create(&path).and_then(|file| {
                let mut encoder =
                    flate2::write::GzEncoder::new(file, flate2::Compression::default());
                encoder.write_all(text.as_bytes())?;
                encoder.finish().map(drop)
            }),
            false => std::fs::write(&path, text),
        };
        written.map_err(|error| format!("cannot write {}: {error}", path.display()))
    }

    fn validate(&self, report: &Json, path: &Path) -> Result<(), String> {
        let (schema, uri) = match &self.settings.schema {
            Some(schema) => {
                let file = self.launch.join(schema);
                let text = std::fs::read_to_string(&file)
                    .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
                (text, format!("file://{}", file.display()))
            }
            None => (
                SCHEMA.to_owned(),
                "nf-iridanext output schema (built in)".to_owned(),
            ),
        };
        let schema: serde_json::Value =
            serde_json::from_str(&schema).map_err(|error| error.to_string())?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| error.to_string())?;
        let instance: serde_json::Value =
            serde_json::from_str(&report.compact()).map_err(|error| error.to_string())?;
        match validator.validate(&instance) {
            Ok(()) => Ok(()),
            Err(error) => Err(format!(
                "Failed to write IRIDA Next JSON output to {}. JSON did not match schema {uri}. \
                 Message: {error}",
                path.display()
            )),
        }
    }

    /// The output: the published files of the tasks by scope, and the metadata of the samples.
    fn report(&self, folder: &Path) -> Result<Json, String> {
        let settings = &self.settings;
        let targets: HashMap<&str, &str> = (self.published.iter())
            .map(|(source, target)| (source.as_str(), target.as_str()))
            .collect();
        let mut ids: Vec<String> = REGISTERED.with(|ids| ids.borrow().clone());
        let mut global = Vec::new();
        let mut samples = Json::Object(Vec::new());
        for task in &self.tasks {
            for parts in &task.declared {
                let sample = sample(parts, &settings.key, task)?;
                if let Some(id) = &sample
                    && !ids.contains(id)
                {
                    ids.push(id.clone());
                }
                let globs = match sample {
                    Some(_) => &settings.samples,
                    None => &settings.global,
                };
                for source in files(parts, task) {
                    let Some(target) = targets.get(source) else {
                        continue;
                    };
                    if !globs.iter().any(|glob| matches(glob, target)) {
                        continue;
                    }
                    let shown = match settings.relativize {
                        true => relative(folder, Path::new(target)),
                        false => (*target).to_owned(),
                    };
                    let entry = Json::object([("path", Json::from(shown))]);
                    match &sample {
                        Some(id) => match samples.get_mut(id) {
                            Some(Json::List(list)) => list.push(entry),
                            _ => samples.insert(id, Json::List(vec![entry])),
                        },
                        None => global.push(entry),
                    }
                }
            }
        }
        let metadata = self.metadata(&ids)?;
        Ok(Json::object([
            (
                "files",
                Json::object([("global", Json::List(global)), ("samples", samples)]),
            ),
            ("metadata", Json::object([("samples", metadata)])),
        ]))
    }

    /// The metadata of the registered samples from the parsers, after flattening and the key
    /// rules.
    fn metadata(&self, ids: &[String]) -> Result<Json, String> {
        let mut found = Json::Object(Vec::new());
        for parser in &self.settings.parsers {
            let Some(target) = self.source(parser) else {
                continue;
            };
            for (id, value) in read(parser, &self.launch.join(target))? {
                if ids.contains(&id) {
                    found.insert(&id, value);
                }
            }
        }
        let Json::Object(samples) = found else {
            return Ok(Json::Object(Vec::new()));
        };
        let mut kept = Vec::new();
        for (id, value) in samples {
            kept.push((id, self.arrange(value)?));
        }
        Ok(Json::Object(kept))
    }

    /// The first published file that a metadata parser reads.
    fn source(&self, parser: &Parser) -> Option<&String> {
        let glob = match parser {
            Parser::Csv { glob, .. } | Parser::Json { glob } => glob,
        };
        (self.published.iter())
            .map(|(_, target)| target)
            .find(|target| glob.as_ref().is_none_or(|glob| matches(glob, target)))
    }

    /// The metadata of one sample after `flatten`, `ignore`, `keep` and `rename`.
    fn arrange(&self, value: Json) -> Result<Json, String> {
        let settings = &self.settings;
        let value = match settings.flatten {
            true => {
                let mut flat = Vec::new();
                flattened("", value, &mut flat);
                Json::Object(flat)
            }
            false => value,
        };
        let Json::Object(entries) = value else {
            return Ok(value);
        };
        let mut result: Vec<(String, Json)> = Vec::new();
        for (key, value) in entries.iter().cloned() {
            if settings.ignore.contains(&key) {
                continue;
            }
            if settings
                .keep
                .as_ref()
                .is_some_and(|keep| !keep.contains(&key))
            {
                continue;
            }
            let renamed = settings.rename.iter().find(|(from, _)| *from == key);
            let name = match renamed {
                Some((from, to)) => {
                    let taken = |name: &str| entries.iter().any(|(known, _)| known == name);
                    if taken(to) || result.iter().any(|(known, _)| known == to) {
                        return Err(format!(
                            "Cannot rename metadata key [{from}] to [{to}], key [{to}] already \
                             exists"
                        ));
                    }
                    to.clone()
                }
                None => key,
            };
            result.push((name, value));
        }
        Ok(Json::Object(result))
    }
}

/// The sample of an output declaration: the id of its meta map, the map that a tuple of more
/// than one part starts with. uwe does not know the variable name of a `val` part.
fn sample(parts: &[Part], key: &str, task: &Task) -> Result<Option<String>, String> {
    let Some(Part::Value(serde_json::Value::Object(meta))) = parts.first() else {
        return Ok(None);
    };
    if parts.len() < 2 {
        return Ok(None);
    }
    match meta.get(key) {
        Some(serde_json::Value::String(id)) => Ok(Some(id.clone())),
        Some(id) => Ok(Some(id.to_string())),
        None => {
            let name = task.record.get("name").and_then(|name| name.as_str());
            let fields: Vec<String> = (meta.iter())
                .map(|(name, value)| match value {
                    serde_json::Value::String(text) => format!("{name}:{text}"),
                    other => format!("{name}:{other}"),
                })
                .collect();
            Err(format!(
                "iridanext.output.files.idkey={key}, but output for task [{}] has map meta \
                 missing key {key}: [{}]",
                name.unwrap_or_default(),
                fields.join(", ")
            ))
        }
    }
}

/// Where the output files of a declaration are, by the patterns of its `path` parts.
fn files<'a>(parts: &'a [Part], task: &'a Task) -> impl Iterator<Item = &'a str> {
    let patterns: Vec<&String> = (parts.iter())
        .flat_map(|part| match part {
            Part::Files(patterns) => patterns.as_slice(),
            _ => &[],
        })
        .collect();
    (task.outputs.iter())
        .filter(move |file| patterns.iter().any(|pattern| matches(pattern, &file.name)))
        .map(|file| file.path.as_str())
}

/// The samples of a metadata file, by id, in row or key order.
fn read(parser: &Parser, path: &Path) -> Result<Vec<(String, Json)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    match parser {
        Parser::Csv {
            column, separator, ..
        } => {
            let options = CsvOptions {
                separator: *separator,
                quote: Some('"'),
                header: Some(None),
                skip: 0,
                strip: true,
            };
            let rows =
                records(&text, &options, false).map_err(|error| error.message.to_string())?;
            let mut samples = Vec::new();
            for row in rows {
                let fields = entries(&row);
                let Some((_, id)) = fields.iter().find(|(name, _)| name == column) else {
                    return Err(format!(
                        "Error: column with idColumn={column} not in CSV {}",
                        path.display()
                    ));
                };
                let others =
                    (fields.iter())
                        .filter(|(name, _)| name != column)
                        .map(|(name, value)| {
                            let value = match value {
                                Value::Null => String::new(),
                                other => other.to_string(),
                            };
                            (name.clone(), Json::from(value))
                        });
                samples.push((id.to_string(), Json::Object(others.collect())));
            }
            Ok(samples)
        }
        Parser::Json { .. } => {
            let value = parse_json(&text).map_err(|error| error.message.to_string())?;
            match ordered_json(&value) {
                Json::Object(entries) => Ok(entries),
                _ => Ok(Vec::new()),
            }
        }
    }
}

/// The entries of nested objects and lists under keys joined with `.`, list places from 1.
fn flattened(prefix: &str, value: Json, flat: &mut Vec<(String, Json)>) {
    let key = |name: &str| match prefix {
        "" => name.to_owned(),
        prefix => format!("{prefix}.{name}"),
    };
    match value {
        Json::Object(entries) => {
            for (name, value) in entries {
                flattened(&key(&name), value, flat);
            }
        }
        Json::List(items) => {
            for (place, value) in items.into_iter().enumerate() {
                flattened(&key(&(place + 1).to_string()), value, flat);
            }
        }
        scalar => flat.push((prefix.to_owned(), scalar)),
    }
}

/// A path relative to a folder, with `..` for the parts of the folder it is not in.
fn relative(folder: &Path, path: &Path) -> String {
    let folder: Vec<Component> = folder.components().collect();
    let path: Vec<Component> = path.components().collect();
    let shared = (folder.iter().zip(&path))
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".to_owned(); folder.len() - shared];
    parts.extend(
        (path[shared..].iter()).map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_with_places() {
        let value = Json::object([(
            "k",
            Json::object([
                ("x", Json::from(1i64)),
                ("y", Json::list([Json::Bool(true), Json::from("a")])),
            ]),
        )]);
        let mut flat = Vec::new();
        flattened("", value, &mut flat);
        let keys: Vec<&str> = flat.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["k.x", "k.y.1", "k.y.2"]);
    }

    #[test]
    fn refuses_remote_metadata() {
        let mut settings = settings(&Config::default()).expect("settings");
        settings.parsers.push(Parser::Json { glob: None });
        let mut irida = Irida {
            settings,
            launch: PathBuf::from("/launch"),
            tasks: Vec::new(),
            published: Vec::new(),
            failure: None,
        };
        irida.notify(&Event::FilePublished {
            source: "/work/meta.json".into(),
            target: "s3://bucket/run/meta.json".into(),
        });
        let message = "nf-iridanext reads sample metadata only from local files, not \
                       s3://bucket/run/meta.json";
        assert_eq!(irida.check(), Err(message.to_owned()));
    }

    #[test]
    fn relativizes_paths() {
        let folder = Path::new("/r/results");
        assert_eq!(relative(folder, Path::new("/r/results/a/b.gz")), "a/b.gz");
        assert_eq!(relative(folder, Path::new("/r/other/c")), "../other/c");
    }
}
