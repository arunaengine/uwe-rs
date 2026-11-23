//! The tool library: tool definitions from definition files and from the tools of every
//! language uwe reads, indexed by id and version, with queries, suggestions and graph checks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tool_definition::edam::{hierarchy, term_id};
use tool_definition::{
    Curation, Definition, FormatMatch, Graph, InputPort, OutputFormat, Ranking, Report, SourceKind,
    Suggestion, Supply, graph, suggest,
};

/// Folders that a scan never enters: build outputs, run folders and hidden folders.
const SKIPPED: [&str; 6] = [
    "target",
    "node_modules",
    "work",
    "results",
    "__pycache__",
    "venv",
];

/// How deep a scan goes below the folder it starts in.
const DEPTH: usize = 8;

/// Tool definitions by id and version, with where each came from and how often it is used.
#[derive(Clone, Debug, Default)]
pub struct ToolLibrary {
    entries: Vec<Entry>,
    uses: BTreeMap<String, u64>,
    skipped: Vec<Skipped>,
}

/// One tool version of the library and the file it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub definition: Definition,
    pub origin: Option<PathBuf>,
}

/// A file that a scan looked at and took no tool from, with the reason in one sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

/// What a query asks of tools; every given field must hold. Formats, operations and topics
/// are EDAM terms, and a more specific term than the one asked for matches too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    /// Words in the id, title, description, keywords or port names, in any case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<SourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// A format of data that an input of the tool takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_format: Option<String>,
    /// A format of data that an output of the tool gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    /// The least trusted curation level to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curation: Option<Curation>,
}

impl ToolLibrary {
    pub fn new() -> ToolLibrary {
        ToolLibrary::default()
    }

    /// Adds a tool version; a version the library holds already keeps its first definition.
    /// Whether it was added.
    pub fn add(&mut self, definition: Definition, origin: Option<PathBuf>) -> bool {
        if self
            .get(&definition.id, Some(&definition.version))
            .is_some()
        {
            return false;
        }
        self.entries.push(Entry { definition, origin });
        true
    }

    /// Adds the tools of one file: a definition file (`.tool.json`, `.tool.yaml` or
    /// `.tool.yml`), or a tool, task, process or rule of a language uwe reads. The number of
    /// tool versions added.
    pub fn add_file(&mut self, path: &Path) -> Result<usize, String> {
        let found = super::extract(path)?;
        let origin = Some(path.to_owned());
        Ok((found.into_iter())
            .filter(|definition| self.add(definition.clone(), origin.clone()))
            .count())
    }

    /// Adds the tools of every file below `folder` that holds some, as `add_file` reads them;
    /// `skipped` lists the files that hold none, and the links and folders a scan leaves out.
    /// The number of tool versions added.
    pub fn scan(&mut self, folder: &Path) -> usize {
        let mut files = Vec::new();
        let root = folder.canonicalize().unwrap_or_else(|_| folder.to_owned());
        candidates(folder, (&root, 0), &mut files, &mut self.skipped);
        files.sort();
        let mut added = 0;
        for path in files {
            match self.add_file(&path) {
                Ok(count) => added += count,
                Err(reason) => self.skipped.push(Skipped { path, reason }),
            }
        }
        added
    }

    /// Every tool version, in the order added.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The definitions of every tool version, in the order added.
    pub fn definitions(&self) -> Vec<Definition> {
        (self.entries.iter())
            .map(|entry| entry.definition.clone())
            .collect()
    }

    /// The tool version `id` at `version`, else the last added version of the tool.
    pub fn get(&self, id: &str, version: Option<&str>) -> Option<&Entry> {
        let mut found = (self.entries.iter()).filter(|entry| entry.definition.id == id);
        match version {
            Some(version) => found.find(|entry| entry.definition.version == version),
            None => found.next_back(),
        }
    }

    /// The files that held no tool, with why.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// Counts one more use of a tool, like a step of a workflow that runs it.
    pub fn count_use(&mut self, id: &str) {
        *self.uses.entry(id.to_owned()).or_default() += 1;
    }

    /// How often each tool is used, by tool id.
    pub fn uses(&self) -> &BTreeMap<String, u64> {
        &self.uses
    }

    /// The tool versions that `query` matches, in the order added.
    pub fn query(&self, query: &Query) -> Vec<&Entry> {
        (self.entries.iter())
            .filter(|entry| matches(&entry.definition, query))
            .collect()
    }

    /// The inputs of the library's tools that can take `supply`, best first. Without uses in
    /// `ranking`, the uses that the library counted rank the tools.
    pub fn suggest_followers(&self, supply: &Supply, ranking: &Ranking) -> Vec<Suggestion> {
        suggest::followers(supply, &self.definitions(), &self.ranked(ranking))
    }

    /// The outputs of the library's tools that can feed `port`, best first.
    pub fn suggest_producers(&self, port: &InputPort, ranking: &Ranking) -> Vec<Suggestion> {
        suggest::producers(port, &self.definitions(), &self.ranked(ranking))
    }

    /// The findings of every node and edge of `graph` against the library's tools.
    pub fn check_graph(&self, graph: &Graph) -> Report {
        graph::check(graph, &self.definitions())
    }

    fn ranked(&self, ranking: &Ranking) -> Ranking {
        let mut ranking = ranking.clone();
        if ranking.uses.is_empty() {
            ranking.uses = self.uses.clone();
        }
        ranking
    }
}

/// Whether a definition meets every field of a query.
fn matches(definition: &Definition, query: &Query) -> bool {
    let edam = hierarchy();
    let kind = |terms: &[String], wanted: &str| {
        let wanted = term_id(wanted);
        (terms.iter()).any(|term| term_id(term) == wanted || edam.is_kind(term_id(term), wanted))
    };
    let text = query.text.as_deref().is_none_or(|text| {
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        let ports = (definition.inputs.iter().map(|port| port.name.as_str()))
            .chain(definition.outputs.iter().map(|port| port.name.as_str()));
        let haystack = [definition.id.as_str(), &definition.title]
            .into_iter()
            .chain(definition.description.as_deref())
            .chain(definition.keywords.iter().map(String::as_str))
            .chain(ports)
            .collect::<Vec<&str>>()
            .join(" ")
            .to_lowercase();
        words.iter().all(|word| haystack.contains(word.as_str()))
    });
    let source = query.source_kind.is_none_or(|wanted| {
        (definition.source.as_ref()).is_some_and(|source| source.kind == wanted)
    });
    let operation =
        (query.operation.as_deref()).is_none_or(|wanted| kind(&definition.operations, wanted));
    let topic = (query.topic.as_deref()).is_none_or(|wanted| kind(&definition.topics, wanted));
    // Data of the format fits an input as the check of a connection says, FASTQ variants too.
    let input = query.input_format.as_deref().is_none_or(|wanted| {
        (definition.inputs.iter()).any(|port| {
            let supply = Supply {
                kind: port.kind,
                shape: Default::default(),
                formats: vec![term_id(wanted).to_owned()],
                facts: Vec::new(),
            };
            let fits = tool_definition::connection::check(&supply, port).format;
            !port.formats.is_empty() && matches!(fits, FormatMatch::Exact | FormatMatch::General)
        })
    });
    let output = query.output_format.as_deref().is_none_or(|wanted| {
        (definition.outputs.iter()).any(|port| match &port.format {
            OutputFormat::Terms(terms) => kind(terms, wanted),
            OutputFormat::Follows(_) => false,
        })
    });
    let curation = query
        .curation
        .is_none_or(|least| definition.curation <= least);
    text && source && operation && topic && input && output && curation
}

/// The files below a folder that may hold tools, by their names and endings. A scan follows
/// no link to a folder, which could lead in a circle, and no link that leads outside `root`.
fn candidates(
    folder: &Path,
    (root, depth): (&Path, usize),
    found: &mut Vec<PathBuf>,
    skipped: &mut Vec<Skipped>,
) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if name.starts_with('.') || (kind.is_dir() && SKIPPED.contains(&name.as_str())) {
            continue;
        }
        let reason = if kind.is_symlink() {
            let inside = path.canonicalize().is_ok_and(|real| real.starts_with(root));
            match (inside, path.is_dir()) {
                (false, _) => "it is a link that leads outside the scanned folder",
                (true, true) => "it is a link to a folder, which a scan does not follow",
                (true, false) => "",
            }
        } else if kind.is_dir() && depth < DEPTH {
            candidates(&path, (root, depth + 1), found, skipped);
            continue;
        } else if kind.is_dir() {
            "it is deeper than a scan goes"
        } else {
            ""
        };
        match reason.is_empty() {
            true if super::is_candidate(&path) => found.push(path),
            true => {}
            false => skipped.push(Skipped {
                path,
                reason: reason.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(id: &str, version: &str, operation: &str) -> Definition {
        serde_json::from_value(json!({
            "id": id, "version": version, "title": format!("{id} tool"),
            "operations": [operation],
            "inputs": [{"name": "reads", "kind": "file", "formats": ["format_1930"]}],
            "outputs": [{"name": "report", "kind": "file", "format": {"terms": ["format_2331"]}}],
            "source": {"kind": "galaxy", "address": "t.xml", "revision": "sha256:00"}
        }))
        .expect("a definition")
    }

    #[test]
    fn indexes_versions() {
        let mut library = ToolLibrary::new();
        assert!(library.add(tool("fastqc", "0.11", "operation_3218"), None));
        assert!(library.add(tool("fastqc", "0.12", "operation_3218"), None));
        assert!(
            !library.add(tool("fastqc", "0.12", "operation_0004"), None),
            "known"
        );
        assert_eq!(library.entries().len(), 2);
        let latest = library.get("fastqc", None).expect("a version");
        assert_eq!(latest.definition.version, "0.12");
        assert!(library.get("fastqc", Some("0.10")).is_none());
    }

    #[test]
    fn scans_without_links() {
        let folder = tempfile::tempdir().expect("a folder");
        let root = folder.path().join("tools");
        let outside = folder.path().join("outside");
        std::fs::create_dir_all(&outside).expect("an outside folder");
        let text = |id: &str| serde_json::to_string(&tool(id, "1", "operation_0004")).unwrap();
        let mut deep = root.clone();
        for level in 0..=DEPTH {
            deep.push(format!("level{level}"));
        }
        std::fs::create_dir_all(&deep).expect("deep folders");
        std::fs::write(root.join("near.tool.json"), text("near")).expect("a tool");
        std::fs::write(deep.join("far.tool.json"), text("far")).expect("a deep tool");
        std::fs::write(outside.join("away.tool.json"), text("away")).expect("an outside tool");
        std::os::unix::fs::symlink(".", root.join("circle")).expect("a link in a circle");
        std::os::unix::fs::symlink(outside.join("away.tool.json"), root.join("away.tool.json"))
            .expect("a link outside");
        let mut library = ToolLibrary::new();
        assert_eq!(library.scan(&root), 1, "only the near tool");
        let reasons: Vec<(String, &str)> = (library.skipped().iter())
            .map(|skipped| {
                let name = skipped.path.file_name().unwrap_or_default();
                (name.to_string_lossy().into_owned(), skipped.reason.as_str())
            })
            .collect();
        for (name, reason) in [
            ("circle", "a link to a folder"),
            ("away.tool.json", "outside the scanned folder"),
            (&*format!("level{DEPTH}"), "deeper than a scan goes"),
        ] {
            let found = reasons
                .iter()
                .any(|(path, why)| path == name && why.contains(reason));
            assert!(found, "{name}: {reasons:?}");
        }
    }

    #[test]
    fn queries_by_edam() {
        let mut library = ToolLibrary::new();
        library.add(tool("fastqc", "0.12", "operation_3218"), None);
        let found = |query: Query| library.query(&query).len();
        // FASTQ-sanger is a kind of FASTQ, which the input takes.
        let sanger = Query {
            input_format: Some("format_1932".into()),
            ..Query::default()
        };
        assert_eq!(found(sanger), 1);
        let bam = Query {
            input_format: Some("format_2572".into()),
            ..Query::default()
        };
        assert_eq!(found(bam), 0);
        let text = Query {
            text: Some("FastQC report".into()),
            source_kind: Some(SourceKind::Galaxy),
            ..Query::default()
        };
        assert_eq!(found(text), 1);
        let curated = Query {
            curation: Some(Curation::Curated),
            ..Query::default()
        };
        assert_eq!(found(curated), 0, "the tool is unchecked");
        let supply = Supply {
            kind: tool_definition::PortKind::File,
            shape: Default::default(),
            formats: vec!["format_1932".into()],
            facts: Vec::new(),
        };
        let followers = library.suggest_followers(&supply, &Ranking::default());
        assert_eq!(followers.len(), 1);
        assert_eq!(followers[0].port, "reads");
    }
}
