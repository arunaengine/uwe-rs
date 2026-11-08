//! The stream ops `observe`, which calls its closures on the items, the close and the error
//! of a stream, and `collect_file`, which writes the entries of its items to files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};
use wir::reserved::{CollectFile, Observe};
use wir::{Body, Closure, Effect};

use crate::callback::Callbacks;
use crate::collecting::sorted;
use crate::engine::Context;
use crate::operator::{CHUNK, Operator, Pull, Step, Turn};
use crate::publishing::{self, Publication};
use crate::streams::Failure;

/// Calls its closures on the items, the close and the error of `source`, and forwards it.
pub struct Observing {
    pub spec: Observe,
    pub callbacks: Callbacks,
}

impl Observing {
    fn call(
        &mut self,
        turn: &Turn<'_>,
        context: &Context<'_>,
        closure: &Closure,
        arguments: &[Json],
    ) -> Result<(), Failure> {
        let body = Body::Closure(closure.clone());
        let names: &[&str] = if arguments.is_empty() { &[] } else { &["item"] };
        let called = self
            .callbacks
            .call(context, &body, (names, arguments), &turn.values);
        called.map(|_| ()).map_err(|why| turn.failed(why))
    }
}

impl Operator for Observing {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            let pulled = match turn.pull("source") {
                Ok(pulled) => pulled,
                Err(failure) => {
                    // The error goes on downstream; a failing error closure says why instead.
                    if let Some(closure) = self.spec.error.clone() {
                        let error = json!({"status": failure.status, "message": failure.message});
                        if let Err(callback) = self.call(turn, context, &closure, &[error]) {
                            let message =
                                format!("{}, after {}", callback.message, failure.message);
                            return Err(Failure { message, ..failure });
                        }
                    }
                    return Err(failure);
                }
            };
            match pulled {
                Pull::Item(item) => {
                    if let Some(closure) = self.spec.item.clone() {
                        self.call(turn, context, &closure, std::slice::from_ref(&item))?;
                    }
                    turn.emit("out", item);
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed => {
                    if let Some(closure) = self.spec.close.clone() {
                        self.call(turn, context, &closure, &[])?;
                    }
                    return Ok(Step::Done);
                }
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }
}

/// Gathers the entries of its items by file name, and writes the files at the close.
pub struct Collecting {
    pub spec: CollectFile,
    pub callbacks: Callbacks,
    entries: BTreeMap<String, Vec<Json>>,
    effects: Vec<Effect>,
    /// The files written at the close, while their deliveries are not acknowledged.
    waiting: Option<(Publication, Vec<Json>)>,
}

impl Collecting {
    pub fn new(spec: CollectFile, effects: &[Effect]) -> Collecting {
        Collecting {
            spec,
            callbacks: Callbacks::default(),
            entries: BTreeMap::new(),
            effects: effects.to_vec(),
            waiting: None,
        }
    }

    /// Files an item: a name and entries for an array of two or more, a File under its name,
    /// and text under the name of the op.
    fn take(&mut self, turn: &Turn<'_>, context: &Context<'_>, item: Json) -> Result<(), Failure> {
        let item = match self.spec.body.clone() {
            Some(body) => (self
                .callbacks
                .call(context, &body, (&["item"], &[item]), &turn.values))
            .map_err(|why| turn.failed(why))?,
            None => item,
        };
        let default = self
            .spec
            .name
            .clone()
            .unwrap_or_else(|| "collect-file.data".into());
        let (name, entries) = match item {
            Json::Null => return Ok(()),
            Json::Array(mut items) if items.len() >= 2 => {
                let name = items.remove(0);
                let name = name
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| turn.failed(format!("the file name {name} is no text")))?;
                (name, items)
            }
            file @ Json::Object(_) => {
                let basename = file
                    .get("basename")
                    .and_then(Json::as_str)
                    .map(str::to_owned);
                (
                    self.spec.name.clone().or(basename).unwrap_or(default),
                    vec![file],
                )
            }
            text => (default, vec![text]),
        };
        if name.is_empty() || name.contains('/') || name == ".." {
            return Err(turn.failed(format!("the file name {name:?} leaves its folder")));
        }
        self.entries.entry(name).or_default().extend(entries);
        Ok(())
    }

    /// The bytes of an entry: a string, or the content of a File.
    fn bytes(turn: &Turn<'_>, context: &Context<'_>, entry: &Json) -> Result<Vec<u8>, Failure> {
        match entry {
            Json::String(text) => Ok(text.clone().into_bytes()),
            Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("File") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                (context.read(location))
                    .map_err(|error| turn.failed(format!("cannot read {location}: {error}")))
            }
            other => Err(turn.failed(format!("the entry {other} is no text or File"))),
        }
    }

    /// Writes every file, then waits for their deliveries to give them in name order.
    fn close(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<(), Failure> {
        let directory = self.spec.directory.as_deref();
        let (folder, outside) = publishing::folder(turn, context, &self.effects, directory)?;
        let mut texts = Vec::new();
        for (name, entries) in std::mem::take(&mut self.entries) {
            let entries = sorted(
                entries,
                &self.spec.sort,
                (&mut self.callbacks, context),
                turn,
            )?;
            let mut text = Vec::new();
            if let Some(seed) = self.spec.seed.as_ref().filter(|seed| !seed.is_empty()) {
                text.extend_from_slice(seed.as_bytes());
                if self.spec.new_line {
                    text.push(b'\n');
                }
            }
            for (at, entry) in entries.iter().enumerate() {
                let entry = Self::bytes(turn, context, entry)?;
                // The first entry keeps its header lines with `keep_header`.
                let kept: Vec<u8> = match self.spec.skip as usize {
                    0 => entry,
                    _ if at == 0 && self.spec.keep_header => entry,
                    skip => (entry.split_inclusive(|byte| *byte == b'\n'))
                        .skip(skip)
                        .flatten()
                        .copied()
                        .collect(),
                };
                text.extend_from_slice(&kept);
                if self.spec.new_line {
                    text.push(b'\n');
                }
            }
            texts.push((name, text));
        }
        if texts.is_empty() {
            self.waiting = Some(Default::default());
            return Ok(());
        }
        std::fs::create_dir_all(&folder)
            .map_err(|error| turn.failed(format!("cannot make {}: {error}", folder.display())))?;
        let (mut publication, mut files) = (Publication::default(), Vec::new());
        for (name, text) in texts {
            let path = folder.join(&name);
            std::fs::write(&path, &text).map_err(|error| {
                turn.failed(format!("cannot write {}: {error}", path.display()))
            })?;
            let size = text.len();
            files.push(publication.file(context, &path, (&name, size), outside.as_deref()));
        }
        publication.ask(context);
        self.waiting = Some((publication, files));
        Ok(())
    }
}

impl Operator for Collecting {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure> {
        let mut moved = false;
        for _ in 0..CHUNK {
            if let Some((publication, files)) = self.waiting.take() {
                if !publication.delivered(context) {
                    self.waiting = Some((publication, files));
                    break;
                }
                files.into_iter().for_each(|file| turn.emit("out", file));
                return Ok(Step::Done);
            }
            match turn.pull("source")? {
                Pull::Item(item) => {
                    self.take(turn, context, item)?;
                    moved = true;
                }
                Pull::Pending => break,
                Pull::Closed => {
                    self.close(turn, context)?;
                    moved = true;
                }
            }
        }
        Ok(if moved { Step::Moved } else { Step::Idle })
    }

    fn waits_for(&self) -> Option<String> {
        self.waiting.as_ref()?.0.awaited()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value as Json, json};

    use crate::mixing::tests::{Fixture, Store, commit, node, steps, steps_in, wired};
    use crate::operator::{Step, make};

    fn groovy(code: &str) -> Json {
        json!({"language": "groovy", "code": code})
    }

    #[test]
    fn observe_calls() {
        let spec = json!({"observe": {"item": groovy("{ assert it > 0 }"), "close": groovy("{ -> true }")}});
        let node = node(spec, &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        commit(
            &mut streams,
            &[("source", json!(1)), ("source", json!(0))],
            &["source"],
        );
        let (emitted, _, step) = steps(operator.as_mut(), &node, &mut streams, &Map::new());
        assert_eq!(
            emitted,
            [json!(1)],
            "an item callback that fails stops before the item"
        );
        assert!(step.unwrap_err().message.contains("assert"));
    }

    /// The files a collect_file with `fields` writes for the items of a closed stream.
    fn collected(fields: Json, items: &[Json]) -> Vec<(String, String)> {
        (written(fields, items).into_iter())
            .map(|(name, bytes)| (name, String::from_utf8(bytes).unwrap()))
            .collect()
    }

    /// The names and bytes of the files a collect_file with `fields` writes.
    fn written(fields: Json, items: &[Json]) -> Vec<(String, Vec<u8>)> {
        let folder = tempfile::tempdir().unwrap();
        let fixture = Fixture {
            files: folder.path().to_owned(),
            ..Fixture::default()
        };
        let context = fixture.context();
        let node = node(json!({"collect_file": fields}), &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let items: Vec<(&str, Json)> = items.iter().map(|item| ("source", item.clone())).collect();
        commit(&mut streams, &items, &[]);
        let mut step =
            |streams: &mut _| steps_in(operator.as_mut(), &node, streams, &Map::new(), &context);
        let (emitted, _, _) = step(&mut streams);
        assert!(emitted.is_empty(), "no file before the close");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        commit(&mut streams, &[], &["source"]);
        let (emitted, _, step) = step(&mut streams);
        assert_eq!(step, Ok(Step::Done));
        (emitted.iter())
            .map(|file| {
                let text = std::fs::read(file["location"].as_str().unwrap()).unwrap();
                (file["basename"].as_str().unwrap().to_owned(), text)
            })
            .collect()
    }

    #[test]
    fn collect_file_order() {
        let items = [
            json!(["b.txt", "2"]),
            json!(["a.txt", "x\ny\n", "z\n"]),
            json!(["b.txt", "1"]),
        ];
        let files = collected(json!({"sort": "index", "new_line": true}), &items);
        let expected = [
            ("a.txt".to_owned(), "x\ny\n\nz\n\n".to_owned()),
            ("b.txt".to_owned(), "2\n1\n".to_owned()),
        ];
        assert_eq!(
            files, expected,
            "files in name order, entries in arrival order"
        );
        let sorted = collected(
            json!({"name": "all.txt", "sort": "natural", "seed": "id"}),
            &[json!("b"), json!("a")],
        );
        assert_eq!(sorted, [("all.txt".to_owned(), "idab".to_owned())]);
        let headers = [json!("h\n1\n"), json!("h\n2\n")];
        let kept = collected(
            json!({"name": "t.csv", "sort": "index", "skip": 1, "keep_header": true}),
            &headers,
        );
        assert_eq!(
            kept,
            [("t.csv".to_owned(), "h\n1\n2\n".to_owned())],
            "one header for the file"
        );
        assert!(
            collected(json!({}), &[]).is_empty(),
            "an empty stream writes no file"
        );
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("in.gz");
        std::fs::write(&path, b"h\n\x1f\x8b\xff\n").unwrap();
        let file =
            json!({"class": "File", "location": path.to_string_lossy(), "basename": "in.gz"});
        let fields = json!({"name": "all.gz", "skip": 1});
        assert_eq!(
            written(fields, &[file]),
            [("all.gz".to_owned(), b"\x1f\x8b\xff\n".to_vec())],
            "File entries are bytes"
        );
    }

    #[test]
    fn collect_file_provider() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("a"), "a\n").unwrap();
        let fixture = Fixture {
            provider: Some(std::sync::Arc::new(Store(folder.path().to_owned()))),
            files: folder.path().join("area"),
            ..Fixture::default()
        };
        let spec = json!({"collect_file": {"name": "all.txt", "sort": "deep"}});
        let node = node(spec, &["source"], &[], "stream");
        let mut streams = wired(&node);
        let mut operator = make(&node).unwrap();
        let stored = json!({"class": "File", "location": "store://a", "basename": "a"});
        commit(
            &mut streams,
            &[("source", stored.clone()), ("source", stored)],
            &["source"],
        );
        let context = fixture.context();
        let (_, _, step) = steps_in(
            operator.as_mut(),
            &node,
            &mut streams,
            &Map::new(),
            &context,
        );
        assert_eq!(step, Ok(Step::Done), "sorted and read through the provider");
        assert_eq!(
            std::fs::read_to_string(folder.path().join("area/op/all.txt")).unwrap(),
            "a\na\n"
        );
    }

    #[test]
    fn collect_file_publish() {
        let folder = tempfile::tempdir().unwrap();
        let fixture = Fixture {
            provider: Some(std::sync::Arc::new(workflow_graph::LocalFiles)),
            files: folder.path().join("area"),
            ..Fixture::default()
        };
        let context = fixture.context();
        let outside = folder.path().join("results");
        let spec =
            json!({"collect_file": {"name": "all.txt", "directory": outside.to_string_lossy()}});
        let mut node = node(spec, &["source"], &[], "stream");
        let run = |node: &crate::plan::Node| {
            let mut streams = wired(node);
            let mut operator = make(node).unwrap();
            commit(&mut streams, &[("source", json!("a"))], &["source"]);
            let first = steps_in(operator.as_mut(), node, &mut streams, &Map::new(), &context);
            (streams, operator, first)
        };
        let why = run(&node).2.2.unwrap_err().message;
        assert!(why.contains("needs a publish effect"), "{why}");
        let other = folder.path().join("other").to_string_lossy().into_owned();
        node.op.effects = vec![wir::Effect::Publish(other)];
        assert!(
            run(&node).2.2.is_err(),
            "the publish effect names another folder"
        );
        let target = folder.path().to_string_lossy().into_owned();
        node.op.effects = vec![wir::Effect::Publish(target)];
        let (mut streams, mut operator, (emitted, _, step)) = run(&node);
        assert_eq!(
            (emitted, step),
            (vec![], Ok(Step::Idle)),
            "no File before its delivery"
        );
        assert!(!outside.exists(), "the op writes nothing outside the run");
        let asked = fixture.deliveries.take();
        let place = outside.join("all.txt").to_string_lossy().into_owned();
        let targets: Vec<&String> = asked.iter().map(|delivery| &delivery.target).collect();
        assert_eq!(targets, [&place]);
        let area = folder.path().join("area");
        assert!(asked[0].location.starts_with(&*area.to_string_lossy()));
        assert!(fixture.deliveries.acknowledge(&asked[0]));
        let (emitted, _, step) = steps_in(
            operator.as_mut(),
            &node,
            &mut streams,
            &Map::new(),
            &context,
        );
        assert_eq!(step, Ok(Step::Done));
        assert_eq!(
            emitted[0]["location"],
            json!(place),
            "the File names the delivered file"
        );
    }
}
