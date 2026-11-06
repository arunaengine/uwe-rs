//! The `gexf` report of nf-prov: a GEXF 1.3 graph of the tasks and the files they read and
//! made, for Gephi.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use super::{Run, file_name};
use crate::report_format::{local_date, now};

/// The node attributes: id, title, type and default.
const ATTRIBUTES: [(&str, &str, &str); 9] = [
    ("cached", "boolean", "false"),
    ("aborted", "boolean", "false"),
    ("node_type", "string", "file"),
    ("path", "string", ""),
    ("processes", "string", ""),
    ("file_size", "long", "-1"),
    ("file_suffix", "string", ""),
    ("created_timestamp", "long", "-1"),
    ("modified_timestamp", "long", "-1"),
];

/// A node of the graph: a task or a file.
struct Node {
    id: usize,
    label: String,
    kind: NodeKind,
}

enum NodeKind {
    Task { cached: bool, process: String },
    File { path: String, file: Option<Facts> },
}

/// What the file system tells of a regular file or a directory.
struct Facts {
    size: Option<u64>,
    suffix: String,
    created: Option<i64>,
    modified: Option<i64>,
}

/// The graph as one line of XML.
pub(super) fn render(run: &Run) -> String {
    let mut graph = Graph {
        counter: 2,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    for task in run.tasks {
        let cached = task.record["status"] == "CACHED";
        let process = task.record["process"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let label = task.record["name"].as_str().unwrap_or_default().to_owned();
        let node = graph.add(label, NodeKind::Task { cached, process });
        for input in &task.inputs {
            let file = graph.file(&input.path, &input.name);
            graph.edge(file, node);
        }
        for output in &task.outputs {
            let file = graph.file(&output.path, file_name(&output.path));
            graph.edge(node, file);
        }
    }
    graph.text()
}

struct Graph {
    /// The next id; nodes and edges share it, and it starts at 2 as in nf-prov.
    counter: usize,
    nodes: Vec<Node>,
    edges: Vec<(usize, usize, usize)>,
}

impl Graph {
    fn add(&mut self, label: String, kind: NodeKind) -> usize {
        let id = self.counter;
        self.counter += 1;
        self.nodes.push(Node { id, label, kind });
        id
    }

    /// The node of a file, which a later task that reads or makes it shares.
    fn file(&mut self, path: &str, label: &str) -> usize {
        let known = self
            .nodes
            .iter()
            .find(|node| matches!(&node.kind, NodeKind::File { path: known, .. } if known == path));
        if let Some(node) = known {
            return node.id;
        }
        let file = facts(path);
        let kind = NodeKind::File {
            path: path.to_owned(),
            file,
        };
        self.add(label.to_owned(), kind)
    }

    fn edge(&mut self, source: usize, target: usize) {
        self.edges.push((self.counter, source, target));
        self.counter += 1;
    }

    fn text(&self) -> String {
        let mut text = String::from(
            "<?xml version='1.0' encoding='UTF-8'?><gexf xmlns=\"http://gexf.net/1.3\" \
             xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xmlns:viz=\"http://gexf.net/1.3/viz\" xsi:schemaLocation=\"http://gexf.net/1.3 \
             http://gexf.net/1.3/gexf.xsd\" version=\"1.3\">",
        );
        let _ = write!(
            text,
            "<meta lastmodifieddate=\"{}\"><creator>GEXF Renderer for nf-prov.</creator>\
             <description>GEXF Renderer for nf-prov. Visualize with https://gephi.org/.\
             </description></meta><graph mode=\"static\" defaultedgetype=\"directed\">\
             <attributes class=\"node\" mode=\"static\">",
            local_date(now())
        );
        for (id, (title, kind, default)) in ATTRIBUTES.iter().enumerate() {
            let options = match *title {
                "node_type" => "<options>[file, task]</options>",
                _ => "",
            };
            let _ = write!(
                text,
                "<attribute id=\"{id}\" title=\"{title}\" type=\"{kind}\"><default>{default}\
                 </default>{options}</attribute>"
            );
        }
        text.push_str("</attributes><attributes class=\"edge\" mode=\"static\"/><nodes>");
        let sizes: Vec<u64> = (self.nodes.iter())
            .filter_map(|node| match &node.kind {
                NodeKind::File {
                    file: Some(facts), ..
                } => facts.size,
                _ => None,
            })
            .collect();
        let (low, high) = (sizes.iter().min(), sizes.iter().max());
        let range = low.zip(high).filter(|(low, high)| low != high);
        for node in &self.nodes {
            let _ = write!(
                text,
                "<node id=\"n{}\" label=\"{}\"><attvalues>",
                node.id,
                escape(&node.label)
            );
            match &node.kind {
                NodeKind::Task { cached, process } => {
                    let _ = write!(
                        text,
                        "<attvalue for=\"0\" value=\"{cached}\"/><attvalue for=\"1\" \
                         value=\"false\"/><attvalue for=\"2\" value=\"task\"/><attvalue \
                         for=\"4\" value=\"{}\"/></attvalues><viz:shape value=\"disc\"/>\
                         <viz:color hex=\"#A7E0E0\"/>",
                        escape(process)
                    );
                }
                NodeKind::File { path, file } => {
                    let _ = write!(
                        text,
                        "<attvalue for=\"2\" value=\"file\"/><attvalue for=\"3\" value=\"{}\"/>",
                        escape(path)
                    );
                    let facts = file.as_ref();
                    let values = [
                        (
                            5,
                            facts
                                .and_then(|facts| facts.size)
                                .map(|size| size.to_string()),
                        ),
                        (6, facts.map(|facts| facts.suffix.clone())),
                        (
                            7,
                            facts
                                .and_then(|facts| facts.created)
                                .map(|time| time.to_string()),
                        ),
                        (
                            8,
                            facts
                                .and_then(|facts| facts.modified)
                                .map(|time| time.to_string()),
                        ),
                    ];
                    for (id, value) in values {
                        if let Some(value) = value.filter(|value| !value.is_empty()) {
                            let _ = write!(
                                text,
                                "<attvalue for=\"{id}\" value=\"{}\"/>",
                                escape(&value)
                            );
                        }
                    }
                    text.push_str(
                        "</attvalues><viz:shape value=\"square\"/><viz:color hex=\"#CA3C66\"/>",
                    );
                    let size = facts.and_then(|facts| facts.size);
                    if let (Some(size), Some((low, high))) = (size, range) {
                        let scaled = 10.0 + (size - low) as f64 / (high - low) as f64 * 90.0;
                        let _ = write!(text, "<viz:size value=\"{}\"/>", double(scaled));
                    }
                }
            }
            text.push_str("</node>");
        }
        text.push_str("</nodes><edges>");
        for (id, source, target) in &self.edges {
            let _ = write!(
                text,
                "<edge id=\"E{id}\" type=\"directed\" source=\"n{source}\" target=\"n{target}\"/>"
            );
        }
        text.push_str("</edges></graph></gexf>");
        text
    }
}

/// The size, suffix and times of a local file, and the suffix `directory` of a folder.
fn facts(path: &str) -> Option<Facts> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.is_dir() {
        return Some(Facts {
            size: None,
            suffix: "directory".to_owned(),
            created: None,
            modified: None,
        });
    }
    let name = file_name(path);
    let name = (name
        .strip_suffix(".gz")
        .or_else(|| name.strip_suffix(".bz2")))
    .unwrap_or(name);
    let suffix = name.rsplit_once('.').map(|(_, suffix)| suffix.to_owned());
    let millis = |time: std::io::Result<std::time::SystemTime>| {
        let since = time.ok()?.duration_since(UNIX_EPOCH).ok()?;
        i64::try_from(since.as_millis()).ok()
    };
    let modified = millis(metadata.modified());
    Some(Facts {
        size: metadata.is_file().then_some(metadata.len()),
        suffix: suffix.unwrap_or_default(),
        created: millis(metadata.created()).or(modified),
        modified,
    })
}

/// A number as Java prints a double: `10.0`, `70.0`, `43.333333333333336`.
fn double(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
