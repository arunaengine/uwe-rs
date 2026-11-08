//! The versioned wir document: sources, named types, graphs, ports and edges, and its parser.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::flow::Flow;
use crate::op::Op;
use crate::types::{Type, TypeDef};

/// The wir version this crate writes and reads. It stays 0.1 until wir covers most of the
/// languages; the README states the rule.
pub const VERSION: &str = "0.1";

/// The versions this crate reads.
pub(crate) const KNOWN: [&str; 1] = [VERSION];

/// The JSON Schema (draft 2020-12) of the JSON form of this version.
pub const SCHEMA: &str = include_str!("../schema.json");

/// One wir document. Its JSON form is described by [`crate::SCHEMA`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub wir_version: String,
    /// The source documents that ops come from or that `native` ops run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeDef>,
    pub graphs: Vec<Graph>,
    /// The name of the graph that a run starts.
    pub entry: String,
}

/// A source document in a workflow language.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub language: Language,
    /// A path relative to the wir document, or a URL.
    pub location: String,
    /// The language version, such as `v1.2` for CWL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A content digest such as `sha256:<hex>`, so a run can prove which source it used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// The workflow languages that wir sources can be written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Cwl,
    Nextflow,
    Snakemake,
    Argo,
    Wdl,
}

/// A named graph: boundary ports, ops and the edges between them. A run, a `call` or a
/// reserved op body runs a graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<Op>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
}

/// A typed input or output of an op or a graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub name: String,
    pub r#type: Type,
    #[serde(default, skip_serializing_if = "Flow::is_value")]
    pub flow: Flow,
    /// The value an input takes when it is bound to null or has no producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

impl Port {
    /// The type of the values an input takes: its own type, and also null when it has a
    /// default.
    pub fn takes(&self) -> Type {
        match &self.default {
            Some(_) => Type::Optional(Box::new(self.r#type.clone())),
            None => self.r#type.clone(),
        }
    }
}

/// A data edge from one producer port to one consumer port. A plain value edge binds the
/// consumer to the producer's value once the producer has finished; the README has the rest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: Endpoint,
    pub to: Endpoint,
}

/// One end of an edge. Without an op it is a graph port: an input of the graph as a source,
/// an output of the graph as a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    pub port: String,
}

/// Why a text is not a wir document of a known version.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("not a wir document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the document has no wir_version string")]
    NoVersion,
    #[error("wir version {found} is not known; this reader knows {VERSION}")]
    Version { found: String },
}

/// Parses the JSON form. The version is checked first, so a newer document fails with a
/// version error instead of an unknown field. It does not verify the graph; see [`crate::verify`].
pub fn parse(text: &str) -> Result<Document, ParseError> {
    let json: serde_json::Value = serde_json::from_str(text)?;
    match json.get("wir_version").and_then(|value| value.as_str()) {
        Some(found) if KNOWN.contains(&found) => Ok(serde_json::from_value(json)?),
        Some(found) => Err(ParseError::Version {
            found: found.to_owned(),
        }),
        None => Err(ParseError::NoVersion),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::op::{Argument, Kind};

    fn text(value: serde_json::Value) -> String {
        value.to_string()
    }

    #[test]
    fn rejects_unknown_versions() {
        let newer = text(json!({"wir_version": "9.0", "graphs": [], "entry": "main", "x": 1}));
        assert!(matches!(parse(&newer), Err(ParseError::Version { found }) if found == "9.0"));
        let missing = text(json!({"graphs": [], "entry": "main"}));
        assert!(matches!(parse(&missing), Err(ParseError::NoVersion)));
    }

    #[test]
    fn rejects_unknown_fields() {
        let op = json!({"path": "a", "kind": {"call": {"graph": "g", "extra": 1}}});
        let document =
            json!({"wir_version": VERSION, "graphs": [{"name": "g", "ops": [op]}], "entry": "g"});
        assert!(matches!(parse(&text(document)), Err(ParseError::Json(_))));
        let argument = json!({"input": "a", "flag": "-i"});
        assert!(serde_json::from_value::<Argument>(argument).is_err());
    }

    #[test]
    fn arguments_keep_form() {
        let command =
            json!(["sort", {"input": "text"}, {"input": "k", "prefix": "-k", "separate": false}]);
        let parsed: Vec<Argument> =
            serde_json::from_value(command.clone()).expect("valid arguments");
        assert_eq!(parsed[1], Argument::input("text"));
        let joined = Argument::Input {
            input: "k".into(),
            prefix: Some("-k".into()),
            separate: false,
            separator: None,
            quote: true,
        };
        assert_eq!(parsed[2], joined);
        assert_eq!(serde_json::to_value(&parsed).expect("serializes"), command);
    }

    #[test]
    fn reserved_kinds_parse() {
        let kind: Kind = serde_json::from_value(json!({"collect": {}})).expect("valid kind");
        assert!(kind.is_reserved());
        let kind: Kind =
            serde_json::from_value(json!({"call": {"graph": "g"}})).expect("valid kind");
        assert!(!kind.is_reserved());
    }
}
