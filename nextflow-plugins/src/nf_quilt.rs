//! nf-quilt 1.0.1: `quilt+s3://` paths through the Quilt provider of storage, and a Quilt
//! package of the published files of a run, with its metadata, README and summary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::value::Value as Script;
use serde_json::{Map, Value, json};
use storage::quilt::Address;
use storage::quilt_package::Push;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["quilt"],
    observer: Some(activate),
    ..Plugin::new("nf-quilt", Kind::FileSystem, State::Implemented)
};

const SUMMARIZE: &str = "*.md,*.html,*.?sv,*.pdf,igv.json,**/multiqc_report.html";
const MESSAGE: &str = "\n${config.get('runName')}: ${meta.get('cmd')}\n";
const README: &str = "\n# ${pkg}\n\n## ${now}\n\n## Run Command\n\n```bash\n${cmd}\n```\n\n\
### Workflow\n\n- **workflow run name**: ```${meta['workflow']?.get('runName')}```\n\
- **scriptFile**: ```${meta['workflow']?.get('scriptFile')}```\n\
- **sessionI**: ```${meta['workflow']?.get('sessionId')}```\n\
- **start**: ```${meta['time_start']}```\n- **complete**: ```${meta['time_complete']}```\n\n\
### Nextflow\n\n${nextflow}\n\n### Processes\n\n`${meta['workflow']?.get('stats')}`\n";

/// The `quilt` scope.
#[derive(Clone, Debug, Default)]
struct Settings {
    /// The scope as JSON, for the `quilt` metadata.
    scope: Map<String, Value>,
    catalog: Option<String>,
    force: bool,
    /// `None` for `meta = false`.
    meta: Option<Map<String, Value>>,
    message: String,
    package: Option<String>,
    /// `None` for `readme = false`.
    readme: Option<String>,
    summarize: Option<Vec<String>>,
}

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let Some(Value::Object(scope)) = config.get(&["quilt"]).map(|value| json_of(&value)) else {
        return Ok(Some(Box::new(Packages::new(Settings::read(Map::new())))));
    };
    Ok(Some(Box::new(Packages::new(Settings::read(scope)))))
}

impl Settings {
    fn read(scope: Map<String, Value>) -> Settings {
        let text = |key: &str| match scope.get(key) {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        };
        let off = |key: &str| scope.get(key) == Some(&Value::Bool(false));
        Settings {
            catalog: text("catalog"),
            force: text("force").is_some_and(|force| force == "true"),
            meta: match scope.get("meta") {
                Some(Value::Object(meta)) => Some(meta.clone()),
                _ if off("meta") => None,
                _ => Some(Map::new()),
            },
            message: text("message").unwrap_or_else(|| MESSAGE.to_owned()),
            package: text("package"),
            readme: (!off("readme")).then(|| text("readme").unwrap_or_else(|| README.to_owned())),
            summarize: (!off("summarize")).then(|| {
                let globs = text("summarize").unwrap_or_else(|| SUMMARIZE.to_owned());
                globs
                    .split(',')
                    .map(|glob| glob.trim().to_owned())
                    .collect()
            }),
            scope,
        }
    }

    /// The settings with the values of the query of an address, which win.
    fn with(&self, address: &Address) -> Settings {
        let mut settings = self.clone();
        for (key, value) in &address.meta {
            match key.as_str() {
                "catalog" => settings.catalog = Some(value.clone()),
                "force" => settings.force = value == "true",
                "message" => settings.message = value.clone(),
                "readme" if value == "false" => settings.readme = None,
                "readme" => settings.readme = Some(value.clone()),
                "summarize" if value == "false" => settings.summarize = None,
                "summarize" => settings.summarize = Some(vec![value.clone()]),
                _ => {}
            }
        }
        if let Some(catalog) = &address.catalog {
            settings.catalog = Some(catalog.clone());
        }
        settings.force |= address.force.as_deref() == Some("true");
        settings
    }
}

/// The packages that the published files of a run go to, by their first address.
struct Packages {
    settings: Settings,
    start: Option<Box<Start>>,
    found: Vec<Address>,
}

impl Packages {
    fn new(settings: Settings) -> Packages {
        Packages {
            settings,
            start: None,
            found: Vec::new(),
        }
    }

    /// Notes the package of a published file; a file at a plain `s3://` address is kept for
    /// its push here, as no Quilt address wrote it.
    fn published(&mut self, source: &str, target: &str) {
        tracing::info!("onFilePublish.dest:{target} <- src:{source}");
        let address = match target.starts_with("quilt+s3://") {
            true => Address::parse(target),
            false => match overlay(target) {
                Some(address) => Ok(address),
                None => return,
            },
        };
        let address = match address {
            Ok(address) => address,
            Err(message) => return tracing::warn!("{message}"),
        };
        if !target.starts_with("quilt+s3://") {
            let local = Path::new(source);
            if !local.is_file() {
                return tracing::warn!("nf-quilt: {target} has no local copy to record");
            }
            if let Err(error) = storage::quilt_package::record(&address.text(), target, local) {
                return tracing::warn!("{error}");
            }
        }
        let known = (self.found.iter())
            .any(|kept| (&kept.bucket, &kept.package) == (&address.bucket, &address.package));
        if !known {
            self.found.push(address);
        }
    }

    fn publish(&self, end: &End) {
        let Some(start) = &self.start else {
            return;
        };
        tracing::info!("publishing: {}", start.run_name);
        for address in &self.found {
            let settings = self.settings.with(address);
            if !end.success && !settings.force {
                tracing::warn!("not publishing: {} [unsuccessful session]", address.package);
                continue;
            }
            let push = push_of(&settings, address, start, end, &now());
            let mut package = Address {
                path: String::new(),
                ..address.clone()
            };
            package.meta.clear();
            match storage::quilt_package::push(push) {
                Ok(_) => {
                    let shown = match &settings.catalog {
                        Some(catalog) => format!(
                            "https://{catalog}/b/{}/packages/{}",
                            address.bucket, address.package
                        ),
                        None => package.text(),
                    };
                    println!("\nSUCCESS: {shown}");
                }
                Err(error) => {
                    eprintln!("publish.FAILED: {}\n{error}", address.package);
                }
            }
        }
    }
}

impl Observer for Packages {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => {
                tracing::info!("`onFlowCreate` {}", start.session_id);
                self.start = Some(start.clone());
            }
            Event::FilePublished { source, target } => self.published(source, target),
            Event::RunEnded(end) => self.publish(end),
            _ => {}
        }
    }
}

/// The package of a plain `s3://` address `s3://<bucket>/<p1>/<p2>/<rest>/<file>`.
fn overlay(target: &str) -> Option<Address> {
    let rest = target.strip_prefix("s3://")?;
    let mut parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    let bucket = parts.first()?.to_string();
    let file = parts.pop().filter(|_| !parts.is_empty())?;
    let middle = &parts[1..];
    let prefix = middle.first().copied().unwrap_or("default_prefix");
    let suffix = middle.get(1).copied().unwrap_or("default_suffix");
    let mut path: Vec<&str> = middle.iter().skip(2).copied().collect();
    path.push(file);
    Some(Address {
        bucket,
        package: format!("{prefix}/{suffix}"),
        path: path.join("/"),
        ..Address::default()
    })
}

/// What the push of a package writes: metadata, the metadata files, README and message.
fn push_of(settings: &Settings, address: &Address, start: &Start, end: &End, now: &Now) -> Push {
    let workflow = workflow(start, end);
    let mut config = match &start.config {
        Value::Object(config) => config.clone(),
        _ => Map::new(),
    };
    config.insert("runName".into(), start.run_name.clone().into());
    let mut meta = Map::new();
    let mut files = Vec::new();
    if let Some(own) = &settings.meta {
        meta.extend(own.clone());
        for (key, value) in &address.meta {
            meta.insert(key.clone(), value.clone().into());
        }
        let mut quilt = settings.scope.clone();
        quilt.remove("meta");
        let mut shown = workflow.clone();
        for key in ["container", "start", "complete", "stats", "commandLine"] {
            shown.remove(key);
        }
        let first = Address {
            meta: Vec::new(),
            ..address.clone()
        };
        meta.extend([
            ("cmd".to_owned(), start.command_line.clone().into()),
            ("now".to_owned(), now.local.clone().into()),
            ("params".to_owned(), start.params.clone()),
            ("quilt".to_owned(), Value::Object(quilt)),
            ("time_start".to_owned(), workflow["start"].clone()),
            ("time_complete".to_owned(), workflow["complete"].clone()),
            ("uri".to_owned(), first.text().into()),
            ("workflow".to_owned(), Value::Object(shown)),
        ]);
        let mut stored = config.clone();
        for key in ["executor", "params", "session"] {
            stored.remove(key);
        }
        let mut strings = workflow.clone();
        for key in ["manifest", "configFiles", "nextflow"] {
            if let Some(value) = strings.get_mut(key) {
                *value = Value::String(value.to_string());
            }
        }
        files.push((
            "nf-quilt/config.json".to_owned(),
            pretty(&crate::service::redacted(&Value::Object(stored)), 0),
        ));
        files.push(("nf-quilt/params.json".to_owned(), pretty(&start.params, 0)));
        files.push((
            "nf-quilt/workflow.json".to_owned(),
            pretty(&Value::Object(strings), 0),
        ));
    }
    let mut shown = meta.clone();
    shown.insert("workflow".into(), Value::Object(workflow));
    let mut variables = json!({
        "cmd": start.command_line, "config": config, "meta": shown, "now": now.local,
        "pkg": settings.package.clone().unwrap_or_else(|| address.package.clone()),
        "nextflow": "  - **version**: ```26.04.6```",
    });
    let message = render(&settings.message, &variables)
        .unwrap_or_else(|| format!("compileMessage.FAILED\n{}", settings.message));
    let message = format!("nf-quilt:{}-{message}", now.day);
    variables["msg"] = message.clone().into();
    if let Some(template) = settings.readme.as_deref().filter(|text| !text.is_empty()) {
        let readme = render(template, &variables)
            .unwrap_or_else(|| format!("compileReadme.FAILED\n{template}"));
        files.push(("README_NF_QUILT.md".to_owned(), readme));
    }
    Push {
        address: Address {
            path: String::new(),
            ..address.clone()
        }
        .text(),
        meta,
        message,
        files: (files.into_iter())
            .map(|(key, text)| (key, text.into_bytes()))
            .collect(),
        summarize: settings.summarize.clone(),
    }
}

/// The `workflow` map of a run, by Nextflow's names.
fn workflow(start: &Start, end: &End) -> Map<String, Value> {
    let script = Path::new(&start.script);
    let name = script
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let home = std::env::var("HOME").unwrap_or_default();
    let value = json!({
        "runName": start.run_name, "sessionId": start.session_id, "scriptName": name,
        "scriptFile": start.script, "projectDir": start.project_dir,
        "projectName": start.project_name, "launchDir": start.launch_dir,
        "workDir": start.work_dir, "homeDir": home, "repository": start.repository,
        "commitId": start.commit_id, "revision": start.revision,
        "commandLine": start.command_line, "profile": start.profile,
        "configFiles": format!("[{}]", start.config_files.join(", ")),
        "manifest": start.manifest, "start": iso(start.start), "complete": iso(end.end),
        "duration": end.end - start.start, "success": end.success,
        "errorMessage": end.error,
        "stats": { "succeededCount": end.counts.succeeded, "cachedCount": end.counts.cached,
            "failedCount": end.counts.failed },
        "nextflow": { "version": "26.04.6" },
    });
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// The time of the push: the day of the message and the `now` of the metadata.
struct Now {
    day: String,
    /// Like `2026-11-06t12-16-36.984583936`.
    local: String,
}

fn now() -> Now {
    let zoned = jiff::Zoned::now();
    Now {
        day: zoned.strftime("%Y-%m-%d").to_string(),
        local: format!(
            "{}.{:09}",
            zoned.strftime("%Y-%m-%dt%H-%M-%S"),
            zoned.subsec_nanosecond()
        ),
    }
}

/// Milliseconds since 1970 as local ISO 8601 with the offset.
fn iso(milliseconds: i64) -> String {
    let Ok(time) = jiff::Timestamp::from_millisecond(milliseconds) else {
        return String::new();
    };
    let zoned = time.to_zoned(jiff::tz::TimeZone::system());
    format!(
        "{}.{:03}{}",
        zoned.strftime("%Y-%m-%dT%H:%M:%S"),
        milliseconds.rem_euclid(1000),
        zoned.strftime("%:z")
    )
}

/// A template with `${name}`, `.get('key')`, `['key']` and `?.` lookups; `None` when an
/// expression is beyond them.
fn render(template: &str, variables: &Value) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find('}')? + at;
        let value = lookup(rest[at + 2..end].trim(), variables)?;
        match value {
            Value::String(text) => out.push_str(&text),
            other => out.push_str(&other.to_string()),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn lookup(expression: &str, variables: &Value) -> Option<Value> {
    let name_end = (expression.find(['.', '[', '?'])).unwrap_or(expression.len());
    let mut value = variables.get(&expression[..name_end])?.clone();
    let mut rest = &expression[name_end..];
    while !rest.is_empty() {
        rest = rest.strip_prefix('?').unwrap_or(rest);
        let (key, after) = if let Some(call) = rest.strip_prefix(".get(") {
            let close = call.find(')')?;
            (quoted(&call[..close])?, &call[close + 1..])
        } else if let Some(index) = rest.strip_prefix('[') {
            let close = index.find(']')?;
            (quoted(&index[..close])?, &index[close + 1..])
        } else {
            let field = rest.strip_prefix('.')?;
            let end = field.find(['.', '[', '?']).unwrap_or(field.len());
            (&field[..end], &field[end..])
        };
        value = value.get(key).cloned().unwrap_or(Value::Null);
        rest = after;
    }
    Some(value)
}

fn quoted(text: &str) -> Option<&str> {
    let text = text.trim();
    (text
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\'')))
    .or_else(|| {
        text.strip_prefix('"')
            .and_then(|text| text.strip_suffix('"'))
    })
}

/// JSON as Jackson's pretty printer writes it: two spaces, `" : "`, arrays on one line.
fn pretty(value: &Value, indent: usize) -> String {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) if map.is_empty() => "{ }".to_owned(),
        Value::Object(map) => {
            let inner = " ".repeat(indent + 2);
            let fields: Vec<String> = (map.iter())
                .map(|(key, value)| {
                    format!(
                        "{inner}{} : {}",
                        Value::from(key.as_str()),
                        pretty(value, indent + 2)
                    )
                })
                .collect();
            format!("{{\n{}\n{pad}}}", fields.join(",\n"))
        }
        Value::Array(items) if items.is_empty() => "[ ]".to_owned(),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(|item| pretty(item, indent)).collect();
            format!("[ {} ]", items.join(", "))
        }
        other => other.to_string(),
    }
}

/// A config value as JSON.
fn json_of(value: &Script) -> Value {
    match value {
        Script::Null => Value::Null,
        Script::Boolean(flag) => Value::Bool(*flag),
        Script::Map(_) => {
            let entries = crate::nf_schema::entries(value);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, json_of(&value)))
                    .collect(),
            )
        }
        Script::List(list) => Value::Array(list.borrow().items.iter().map(json_of).collect()),
        other => Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_templates() {
        let variables = json!({ "config": { "runName": "loving_northcutt" },
            "meta": { "cmd": "nextflow run main.nf", "workflow": { "runName": "x" } } });
        assert_eq!(
            render(MESSAGE, &variables).as_deref(),
            Some("\nloving_northcutt: nextflow run main.nf\n")
        );
        let text =
            "${meta['workflow']?.get('runName')} ${meta.missing?.get('a')} ${config.runName}";
        assert_eq!(
            render(text, &variables).as_deref(),
            Some("x null loving_northcutt")
        );
        assert_eq!(render("${meta.get(a)}", &variables), None);
    }

    #[test]
    fn prints_like_jackson() {
        let value = json!({ "plugins": ["nf-quilt@1.0.1"], "env": {}, "quilt": { "meta": { "pipeline": "probe" } } });
        let expected = "{\n  \"env\" : { },\n  \"plugins\" : [ \"nf-quilt@1.0.1\" ],\n  \"quilt\" : {\n    \
                        \"meta\" : {\n      \"pipeline\" : \"probe\"\n    }\n  }\n}";
        assert_eq!(pretty(&value, 0), expected);
        assert_eq!(pretty(&json!({}), 0), "{ }");
    }

    #[test]
    fn maps_plain_addresses() {
        let address = overlay("s3://bucket/test/probe/out/a.txt").expect("a package");
        assert_eq!(
            (
                address.bucket.as_str(),
                address.package.as_str(),
                address.path.as_str()
            ),
            ("bucket", "test/probe", "out/a.txt")
        );
        let short = overlay("s3://bucket/results/a.txt").expect("a package");
        assert_eq!(
            (short.package.as_str(), short.path.as_str()),
            ("results/default_suffix", "a.txt")
        );
        let top = overlay("s3://bucket/a.txt").expect("a package");
        assert_eq!(top.package, "default_prefix/default_suffix");
        assert!(overlay("/home/me/a.txt").is_none());
    }

    fn run() -> (Start, End) {
        let start = Start {
            run_name: "loving_northcutt".into(),
            session_id: "619cf0cb-361f-476c-85e0-995e98fb6f1e".into(),
            launch_dir: "/scratch".into(),
            work_dir: "/scratch/work".into(),
            project_dir: "/scratch".into(),
            script: "/scratch/main.nf".into(),
            project_name: "main.nf".into(),
            repository: None,
            commit_id: None,
            revision: None,
            command_line: "nextflow run main.nf -ansi-log false".into(),
            profile: "standard".into(),
            config_files: vec!["/scratch/nextflow.config".into()],
            params: json!({}),
            config: json!({ "plugins": ["nf-quilt@1.0.1"], "params": {}, "workDir": "work",
                "aws": { "accessKey": "AKIAEXAMPLE", "secretKey": "not-a-real-secret" } }),
            manifest: json!({}),
            processes: Vec::new(),
            start: 1_793_963_795_486,
            resume: false,
        };
        let end = End {
            success: true,
            error: None,
            counts: Default::default(),
            task: None,
            outputs: Vec::new(),
            end: 1_793_963_796_775,
        };
        (start, end)
    }

    #[test]
    fn assembles_push() {
        let mut scope = Map::new();
        scope.insert("meta".into(), json!({ "pipeline": "probe" }));
        let settings = Settings::read(scope);
        let address = Address::parse("quilt+s3://nfquilt-test#package=test/probe&path=out/a.txt");
        let address = address.expect("an address");
        let now = Now {
            day: "2026-11-06".into(),
            local: "2026-11-06t12-16-36.984583936".into(),
        };
        let (start, end) = run();
        let push = push_of(&settings, &address, &start, &end, &now);
        assert_eq!(push.address, "quilt+s3://nfquilt-test#package=test%2fprobe");
        assert_eq!(
            push.message,
            "nf-quilt:2026-11-06-\nloving_northcutt: nextflow run main.nf -ansi-log false\n"
        );
        let keys: Vec<&str> = push.meta.keys().map(String::as_str).collect();
        let expected = [
            "cmd",
            "now",
            "params",
            "pipeline",
            "quilt",
            "time_complete",
            "time_start",
            "uri",
            "workflow",
        ];
        assert_eq!(keys, expected);
        let uri = "quilt+s3://nfquilt-test#package=test%2fprobe&path=out%2fa.txt";
        assert_eq!(
            (&push.meta["uri"], &push.meta["quilt"]),
            (&json!(uri), &json!({}))
        );
        let workflow = push.meta["workflow"].as_object().expect("a map");
        assert!(workflow.contains_key("runName") && !workflow.contains_key("commandLine"));
        assert!(!workflow.contains_key("stats") && !workflow.contains_key("start"));
        let files: Vec<&str> = push.files.iter().map(|(key, _)| key.as_str()).collect();
        let names = [
            "nf-quilt/config.json",
            "nf-quilt/params.json",
            "nf-quilt/workflow.json",
            "README_NF_QUILT.md",
        ];
        assert_eq!(files, names);
        let config = String::from_utf8_lossy(&push.files[0].1).into_owned();
        assert!(
            !config.contains("\"params\"") && config.contains("\"runName\" : \"loving_northcutt\"")
        );
        assert!(!config.contains("not-a-real-secret"), "{config}");
        assert!(config.contains("\"secretKey\" : \"[secret]\""), "{config}");
        assert_eq!(push.files[1].1, b"{ }");
        let readme = String::from_utf8_lossy(&push.files[3].1).into_owned();
        assert!(readme.starts_with("\n# test/probe\n\n## 2026-11-06t12-16-36.984583936\n"));
        assert!(
            readme.contains("- **workflow run name**: ```loving_northcutt```"),
            "{readme}"
        );
        let off = Settings::read(Map::from_iter([("meta".to_owned(), json!(false))]));
        let bare = push_of(&off, &address, &start, &end, &now);
        assert!(bare.meta.is_empty());
        assert_eq!(bare.files.len(), 1, "only the README");
    }
}
