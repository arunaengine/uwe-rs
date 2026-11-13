//! nf-blocks 0.11.0-beta.1, step 1: workflow outputs published into `cas://` stores, the
//! blocks and Store Log entry of the run, and the `fromStore` channel factory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{PathValue, Value};
use serde_json::Value as Json;
use storage::cbor::{Cid, Node};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{End, Event, Observer, Start};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["fromStore"],
    call: Some(call),
    factories: &["fromStore"],
    scopes: &["cas", "lineage"],
    observer: Some(activate),
    ..Plugin::new("nf-blocks", Kind::FileSystem, State::Partial)
};

const USAGE: &str = "`fromStore` takes `selection: <address>`, or `run: <ref>` with `output: \
                     <name>`; optionally `where: [...]` and `records: true`.";

/// Hands the stores of the `cas` scope to the `cas://` provider, checks `outputDir`, and
/// makes the observer that writes the blocks of the run.
fn activate(
    config: &Config,
    _: &std::path::Path,
    environment: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |path: &[&str]| {
        (config.get(path))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    let mut stores = Vec::new();
    if let Some(table) = config.get(&["cas", "stores"]) {
        for (alias, store) in crate::nf_schema::entries(&table) {
            let valid = alias.len() <= 32
                && alias.starts_with(|c: char| c.is_ascii_lowercase())
                && alias
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
                && Cid::parse(&alias).is_none();
            if !valid {
                return Err(format!("nf-blocks: `{alias}` is not a valid store alias"));
            }
            let location = crate::nf_schema::entries(&store)
                .into_iter()
                .find(|(key, _)| key == "location")
                .map(|(_, value)| value.to_string())
                .ok_or_else(|| format!("nf-blocks: cas.stores.{alias}.location is required"))?;
            stores.push((alias, location));
        }
    }
    let lineage = text(&["lineage", "store", "location"]);
    let writable = lineage
        .as_deref()
        .and_then(|location| location.strip_prefix("cas://"));
    let Some(writable) = writable.map(|alias| alias.trim_end_matches('/').to_owned()) else {
        return Err("nf-blocks needs lineage.store.location = 'cas://<alias>'".to_owned());
    };
    match text(&["outputDir"]) {
        None => tracing::warn!(
            "nf-blocks: uwe cannot set outputDir; set outputDir = 'cas://{writable}' to publish there"
        ),
        Some(output) if output.trim_end_matches('/') == format!("cas://{writable}") => {}
        Some(output) => {
            return Err(format!(
                "nf-blocks: outputDir must be cas://{writable}, the store of lineage.store.location, not {output}"
            ));
        }
    }
    let resolve = match config.get(&["cas", "resolve"]) {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        _ => Vec::new(),
    };
    let signer = text(&["cas", "asserted_by"])
        .map(|name| (name, "cas.asserted_by"))
        .or_else(|| {
            let name = environment("NXF_BLOCKS_USER").filter(|name| !name.is_empty());
            name.map(|name| (name, "NXF_BLOCKS_USER"))
        });
    let asserted_by = match signer {
        Some((name, source)) if name != "anonymous" => {
            println!("nf-blocks: signing as {name} (from {source})");
            name
        }
        _ => {
            tracing::warn!(
                "nf-blocks: no cas.asserted_by or NXF_BLOCKS_USER; signing as anonymous"
            );
            "anonymous".to_owned()
        }
    };
    let locations: Vec<String> = stores
        .iter()
        .map(|(_, location)| location.clone())
        .collect();
    storage::cas::use_scope(storage::cas::Options {
        stores,
        resolve,
        writable: Some(writable.clone()),
    });
    Ok(Some(Box::new(Lineage {
        writable,
        asserted_by,
        pipeline: text(&["cas", "pipeline"]),
        locations,
        start: None,
        made: Vec::new(),
    })))
}

/// Collects what the run made, and writes its blocks when it ends.
struct Lineage {
    writable: String,
    asserted_by: String,
    pipeline: Option<String>,
    /// The locations of the stores, which the config text of the manifest hides.
    locations: Vec<String>,
    start: Option<Box<Start>>,
    /// Each output file of a task with the task's tag, hash and process.
    made: Vec<(String, Node)>,
}

impl Observer for Lineage {
    fn notify(&mut self, event: &Event) {
        match event {
            Event::RunStarted(start) => self.start = Some(start.clone()),
            Event::TaskCompleted(task) => {
                let field = |key: &str| {
                    task.record
                        .get(key)
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                };
                let made = map([
                    ("tag", Node::Text(field("tag").to_owned())),
                    ("hash", Node::Text(task.hash.clone())),
                    ("process", Node::Text(field("process").to_owned())),
                ]);
                for output in &task.outputs {
                    self.made.push((output.path.clone(), made.clone()));
                }
            }
            Event::RunEnded(end) => {
                if let Err(error) = self.finish(end) {
                    tracing::warn!("nf-blocks: the run was not recorded in the store: {error}");
                }
            }
            _ => {}
        }
    }
}

impl Lineage {
    /// Writes the items, collections, input set, lineage, manifest and completion of the run,
    /// then its Store Log entry, so that the entry never names a missing block.
    fn finish(&self, end: &End) -> Result<(), String> {
        let start = self.start.as_ref().ok_or("the run did not start")?;
        let put = |node: &Node| storage::cas::put_block(node).map_err(|error| error.to_string());
        let signed = || ("asserted_by", Node::Text(self.asserted_by.clone()));
        let manifest = put(&self.manifest(start))?;
        let mut collections = Vec::new();
        let mut contents = Vec::new();
        let mut lineage = Vec::new();
        let prefix = format!("cas://{}/", self.writable);
        for output in &end.outputs {
            let (mut items, mut paths) = (Vec::new(), Vec::new());
            for target in output
                .files
                .iter()
                .filter(|target| target.starts_with(&prefix))
            {
                let found = storage::cas::coordinate(target).map_err(|error| error.to_string())?;
                let Some((content, size)) = found else {
                    continue;
                };
                let name = target.rsplit('/').next().unwrap_or_default();
                let leaf = map([
                    ("kind", Node::Text("Leaf".into())),
                    ("name", Node::Text(name.to_owned())),
                    ("size", Node::Integer(i128::from(size))),
                    ("reason", Node::Null),
                    ("address", Node::Link(content)),
                ]);
                let item = put(&map([
                    ("kind", Node::Text("OutputItem".into())),
                    ("value", leaf),
                    ("schema", Node::Integer(2)),
                ]))?;
                let tasks: Vec<Node> = (self.made.iter())
                    .filter(|(path, _)| path.rsplit('/').next() == Some(name))
                    .map(|(_, task)| task.clone())
                    .take(1)
                    .collect();
                lineage.push(map([
                    ("item", Node::Link(item)),
                    ("tasks", Node::List(tasks)),
                    ("inputs", Node::List(Vec::new())),
                ]));
                contents.push(Node::Link(content));
                items.push(Node::Link(item));
                paths.push(Node::List(vec![Node::Text(
                    target[prefix.len()..].to_owned(),
                )]));
            }
            if items.is_empty() {
                continue;
            }
            collections.push(Node::Link(put(&map([
                ("run", Node::Link(manifest)),
                ("kind", Node::Text("OutputCollection".into())),
                ("name", Node::Text(output.name.clone())),
                ("items", Node::List(items)),
                ("paths", Node::List(paths)),
                ("schema", Node::Integer(1)),
                signed(),
            ]))?));
        }
        let inputs = put(&map([
            ("kind", Node::Text("InputSet".into())),
            ("inputs", Node::List(Vec::new())),
            ("schema", Node::Integer(2)),
            signed(),
        ]))?;
        let lineage = put(&map([
            ("kind", Node::Text("ItemLineage".into())),
            ("items", Node::List(lineage)),
            ("schema", Node::Integer(1)),
            ("input_set", Node::Link(inputs)),
            signed(),
        ]))?;
        let anomalies = [
            "declined",
            "unjoined",
            "unaddressed",
            "unresolvable",
            "never_published",
        ]
        .map(|key| (key, Node::Integer(0)));
        let completion = put(&map([
            ("run", Node::Link(manifest)),
            ("kind", Node::Text("RunCompletion".into())),
            ("error", end.error.clone().map_or(Node::Null, Node::Text)),
            ("schema", Node::Integer(3)),
            (
                "status",
                Node::Text(if end.success { "succeeded" } else { "failed" }.into()),
            ),
            ("anomalies", map(anomalies)),
            ("input_set", Node::Link(inputs)),
            ("providers", map([("head-node", Node::List(contents))])),
            ("started_at", Node::Text(utc(start.start))),
            signed(),
            ("collections", Node::List(collections)),
            (
                "exit_status",
                Node::Integer(if end.success { 0 } else { 1 }),
            ),
            ("finished_at", Node::Text(utc(end.end))),
            ("item_lineage", Node::Link(lineage)),
            ("possibly_incomplete", Node::Bool(false)),
        ]))?;
        storage::cas::put_log("run", &completion).map_err(|error| error.to_string())
    }

    fn manifest(&self, start: &Start) -> Node {
        let config = crate::service::redacted(&start.config);
        let mut config = serde_json::to_string_pretty(&config).unwrap_or_default();
        for location in &self.locations {
            config = config.replace(&format!("\"{location}\""), "\"[redacted-location]\"");
        }
        let text = |value: &Option<String>| value.clone().map_or(Node::Null, Node::Text);
        let pipeline = (self.pipeline.clone())
            .or_else(|| start.manifest["name"].as_str().map(str::to_owned))
            .unwrap_or_else(|| start.project_name.clone());
        map([
            ("kind", Node::Text("RunManifest".into())),
            ("config", Node::Text(config)),
            ("params", node(&start.params)),
            ("schema", Node::Integer(1)),
            ("script", Node::Null),
            ("resumed", Node::Bool(false)),
            ("pipeline", Node::Text(pipeline)),
            ("revision", text(&start.revision)),
            ("run_name", Node::Text(start.run_name.clone())),
            ("commit_id", text(&start.commit_id)),
            ("repository", text(&start.repository)),
            ("session_id", Node::Text(start.session_id.clone())),
            ("started_at", Node::Text(utc(start.start))),
            ("asserted_by", Node::Text(self.asserted_by.clone())),
            ("nf_run_hash", Node::Null),
            ("nextflow_version", Node::Text("26.04.6".into())),
        ])
    }
}

/// `channel.fromStore(run: <ref>, output: <name>, where: [...], records: <bool>)`.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let thrown = |message: String| Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    };
    if name != "fromStore" {
        return Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-blocks`"),
            span: Default::default(),
        });
    }
    let named = match arguments.as_slice() {
        [options @ Value::Map(_)] => crate::nf_schema::entries(options),
        _ => return Err(thrown(USAGE.to_owned())),
    };
    let get = |key: &str| {
        named
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    if get("selection").is_some() {
        return Err(thrown(
            "uwe does not read nf-blocks Selections yet".to_owned(),
        ));
    }
    let (Some(reference), Some(output)) = (get("run"), get("output")) else {
        return Err(thrown(USAGE.to_owned()));
    };
    match get("records") {
        None | Some(Value::Boolean(false)) => {}
        Some(Value::Boolean(true)) => tracing::warn!("nf-blocks: records are plain maps in uwe"),
        Some(_) => return Err(thrown("`records` takes true or false".to_owned())),
    }
    let pipeline = get("pipeline").map(|value| value.to_string());
    let wanted: Vec<(String, String)> = match get("where") {
        Some(filter @ Value::Map(_)) => (crate::nf_schema::entries(&filter).into_iter())
            .map(|(key, value)| (key, value.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    let found = items(
        &reference.to_string(),
        pipeline.as_deref(),
        &output.to_string(),
    )
    .map_err(thrown)?;
    let launch = context.host.launch_dir();
    let mut values = Vec::new();
    for (item, value) in found {
        if wanted.iter().all(|(key, text)| matches(&value, key, text)) {
            values.push(value_of(&value, &item, &launch).map_err(thrown)?);
        }
    }
    Ok(Value::list(values))
}

/// The items of an output of a run, with their CIDs and values.
fn items(
    reference: &str,
    pipeline: Option<&str>,
    output: &str,
) -> Result<Vec<(Cid, Node)>, String> {
    let read = |cid: &Cid| {
        (storage::cas::block(cid).map_err(|error| error.to_string()))?
            .ok_or_else(|| format!("the block {cid} is in no store"))
    };
    let kind = |node: &Node| node.get("kind").and_then(Node::text).map(str::to_owned);
    let runs = || storage::cas::runs().map_err(|error| error.to_string());
    let completion = match reference {
        "latest" => {
            let pipeline =
                pipeline.ok_or("`run: 'latest'` needs `pipeline: '<pipeline identity>'`")?;
            let mut found = None;
            for cid in runs()? {
                let completion = read(&cid)?;
                let Some(Node::Link(run)) = completion.get("run") else {
                    continue;
                };
                if read(run)?.get("pipeline").and_then(Node::text) == Some(pipeline) {
                    found = Some(completion);
                    break;
                }
            }
            found.ok_or_else(|| format!("no run of the pipeline {pipeline} is in the stores"))?
        }
        reference if reference.starts_with("lid://") => {
            return Err(
                "uwe has no Nextflow lineage store; name the run by its cas:// address".into(),
            );
        }
        reference => {
            let cid = reference.strip_prefix("cas://").unwrap_or(reference);
            let cid = Cid::parse(cid.trim_end_matches('/'))
                .ok_or_else(|| format!("{reference} names no run"))?;
            let block = read(&cid)?;
            match kind(&block).as_deref() {
                Some("RunCompletion") => block,
                Some("RunManifest") => {
                    let mut found = None;
                    for completion in runs()? {
                        let node = read(&completion)?;
                        if node.get("run") == Some(&Node::Link(cid)) {
                            found = Some(node);
                            break;
                        }
                    }
                    found.ok_or_else(|| format!("the run {reference} has no RunCompletion"))?
                }
                _ => return Err(format!("{reference} is not a run")),
            }
        }
    };
    let collections = match completion.get("collections") {
        Some(Node::List(links)) => links.clone(),
        _ => Vec::new(),
    };
    for link in collections {
        let Node::Link(cid) = link else { continue };
        let collection = read(&cid)?;
        if collection.get("name").and_then(Node::text) != Some(output) {
            continue;
        }
        let Some(Node::List(items)) = collection.get("items") else {
            return Ok(Vec::new());
        };
        let mut found = Vec::new();
        for item in items {
            let Node::Link(item) = item else { continue };
            let block = read(item)?;
            found.push((*item, block.get("value").cloned().unwrap_or(Node::Null)));
        }
        return Ok(found);
    }
    Err(format!("the run has no output `{output}`"))
}

/// Whether the first map of an item has `key` (dotted for nested maps) with this text.
fn matches(value: &Node, key: &str, text: &str) -> bool {
    let first = match value {
        Node::Map(_) => Some(value),
        Node::List(items) => items.iter().find(|item| matches!(item, Node::Map(_))),
        _ => None,
    };
    let mut found = first;
    for part in key.split('.') {
        found = found.and_then(|node| node.get(part));
    }
    match found {
        Some(Node::Text(value)) => value == text,
        Some(Node::Integer(value)) => value.to_string() == text,
        Some(Node::Bool(value)) => value.to_string() == text,
        _ => false,
    }
}

/// An item as the script sees it, every leaf a path or, when declined, null.
fn value_of(node: &Node, item: &Cid, launch: &str) -> Result<Value, String> {
    if node.get("kind").and_then(Node::text) == Some("Leaf") {
        let name = node.get("name").and_then(Node::text).unwrap_or_default();
        return match (node.get("address"), node.get("reason")) {
            (Some(Node::Link(content)), _) => {
                let path = format!("cas://{content}/{name}");
                Ok(Value::Path(Rc::new(PathValue::parse(&path, launch))))
            }
            (_, Some(Node::Text(_))) => Ok(Value::Null),
            _ => Err(format!("the leaf {name} of the item {item} has no address")),
        };
    }
    Ok(match node {
        Node::Null => Value::Null,
        Node::Bool(flag) => Value::Boolean(*flag),
        Node::Integer(number) => match i32::try_from(*number) {
            Ok(small) => Value::integer(small),
            Err(_) => Value::Number(Number::Long(i64::try_from(*number).unwrap_or(i64::MAX))),
        },
        Node::Float(number) => Value::Number(Number::Double(*number)),
        Node::Text(text) => Value::string(text.as_str()),
        Node::Bytes(_) => Value::Null,
        Node::Link(cid) => Value::string(format!("cas://{cid}").as_str()),
        Node::List(items) => {
            let items: Result<Vec<Value>, String> = items
                .iter()
                .map(|item_node| value_of(item_node, item, launch))
                .collect();
            Value::list(items?)
        }
        Node::Map(entries) => {
            let mut values = Vec::new();
            for (key, value) in entries {
                values.push((Value::string(key.as_str()), value_of(value, item, launch)?));
            }
            Value::map(values)
        }
    })
}

fn map<const N: usize>(entries: [(&str, Node); N]) -> Node {
    Node::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// JSON as an IPLD value.
fn node(value: &Json) -> Node {
    match value {
        Json::Null => Node::Null,
        Json::Bool(flag) => Node::Bool(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => Node::Integer(i128::from(integer)),
            None => Node::Float(number.as_f64().unwrap_or(0.0)),
        },
        Json::String(text) => Node::Text(text.clone()),
        Json::Array(items) => Node::List(items.iter().map(node).collect()),
        Json::Object(entries) => Node::Map(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), node(value)))
                .collect(),
        ),
    }
}

/// Milliseconds since 1970 as UTC ISO 8601 with milliseconds and `Z`.
fn utc(milliseconds: i64) -> String {
    let Ok(time) = jiff::Timestamp::from_millisecond(milliseconds) else {
        return String::new();
    };
    let utc = time.to_zoned(jiff::tz::TimeZone::UTC);
    format!(
        "{}.{:03}Z",
        utc.strftime("%Y-%m-%dT%H:%M:%S"),
        milliseconds.rem_euclid(1000)
    )
}
