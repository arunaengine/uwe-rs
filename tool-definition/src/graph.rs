//! The check of a whole graph of tool nodes and edges: all findings per node and per edge.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::conditions::Fact;
use crate::connection::{self, Check, FormatMatch, Supply};
use crate::definition::{Definition, InputPort};
use crate::finding::{Aspect, Finding, Fit, worst};
use crate::parameters;

/// Tool nodes and the edges between their ports.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// One use of a tool version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub tool: String,
    pub version: String,
    /// The parameter values; null or absent is checked as no values, `{}`.
    #[serde(default, skip_serializing_if = "Json::is_null")]
    pub parameters: Json,
    /// Inputs fed from outside the graph, like workflow inputs, by input name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub given: BTreeMap<String, Supply>,
}

/// A connection from an output of one node to an input of another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: End,
    pub to: End,
    /// What else is known about the data on this edge, like a user assertion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<Fact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct End {
    pub node: String,
    pub port: String,
}

/// The findings of a graph: nodes and edges in the order of the graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub fit: Fit,
    pub nodes: Vec<NodeReport>,
    pub edges: Vec<Check>,
}

/// The findings of one node: its tool, parameters, given inputs and unconnected inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReport {
    pub id: String,
    pub fit: Fit,
    /// The node maps over a list; its outputs are lists with a sample id per item.
    pub scatter: bool,
    pub findings: Vec<Finding>,
}

/// Checks every node and edge of `graph` against the tool versions of `tools`. Nodes are
/// checked in the order of their edges, so formats that follow an input resolve upstream first.
pub fn check(graph: &Graph, tools: &[Definition]) -> Report {
    let tool = |node: &Node| {
        tools
            .iter()
            .find(|tool| tool.id == node.tool && tool.version == node.version)
    };
    let index: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(at, node)| (node.id.as_str(), at))
        .collect();
    let (order, cyclic) = ordered(graph, &index);
    let mut outputs: Vec<Option<BTreeMap<String, Supply>>> = vec![None; graph.nodes.len()];
    let mut nodes: Vec<Option<NodeReport>> = vec![None; graph.nodes.len()];
    let mut edges: Vec<Option<Check>> = vec![None; graph.edges.len()];
    for at in order {
        let node = &graph.nodes[at];
        let mut findings = Vec::new();
        if cyclic[at] {
            let reason = format!("node {} is in a cycle or after one", node.id);
            findings.push(Finding::new(Aspect::Connection, Fit::DoesNotFit, reason));
        }
        let Some(definition) = tool(node) else {
            let reason = format!("tool {} version {} is not known", node.tool, node.version);
            findings.push(Finding::new(Aspect::Connection, Fit::DoesNotFit, reason));
            for (number, edge) in graph.edges.iter().enumerate() {
                if edge.to.node == node.id {
                    edges[number] =
                        Some(broken(format!("the tool of node {} is not known", node.id)));
                }
            }
            nodes[at] = Some(report(node, false, findings));
            continue;
        };
        let values = match &node.parameters {
            Json::Null => Json::Object(Default::default()),
            values => values.clone(),
        };
        findings.extend(parameters::check(&definition.parameters, &values));
        let mut inputs = BTreeMap::new();
        let mut scatter = false;
        for (name, supply) in &node.given {
            let Some(port) = definition.input(name) else {
                let reason = format!("the tool has no input {name}");
                findings.push(Finding::new(Aspect::Connection, Fit::DoesNotFit, reason));
                continue;
            };
            let given = connection::check(supply, port);
            scatter |= given.scatter;
            findings.extend(given.findings);
            inputs.insert(name.clone(), supply.clone());
        }
        for (number, edge) in graph.edges.iter().enumerate() {
            if edge.to.node != node.id {
                continue;
            }
            let Some(port) = definition.input(&edge.to.port) else {
                edges[number] = Some(broken(format!("the tool has no input {}", edge.to.port)));
                continue;
            };
            let Some(mut supply) = supplied(graph, tools, &index, &outputs, &edge.from) else {
                let reason = format!(
                    "there is no output {} of node {}",
                    edge.from.port, edge.from.node
                );
                edges[number] = Some(broken(reason));
                continue;
            };
            supply.facts.extend(edge.facts.iter().cloned());
            let checked = connection::check(&supply, port);
            scatter |= checked.scatter;
            edges[number] = Some(checked);
            inputs.insert(edge.to.port.clone(), supply);
        }
        for port in &definition.inputs {
            if port.required && port.default.is_none() && !inputs.contains_key(&port.name) {
                findings.extend(unconnected(port, &node.parameters, &values));
            }
        }
        outputs[at] = Some(resolved(definition, &inputs, scatter));
        nodes[at] = Some(report(node, scatter, findings));
    }
    for (number, edge) in graph.edges.iter().enumerate() {
        if edges[number].is_none() {
            edges[number] = Some(broken(format!("there is no node {}", edge.to.node)));
        }
    }
    let nodes: Vec<NodeReport> = nodes.into_iter().flatten().collect();
    let edges: Vec<Check> = edges.into_iter().flatten().collect();
    let fit = nodes
        .iter()
        .map(|node| node.fit)
        .chain(edges.iter().map(|edge| edge.fit))
        .max()
        .unwrap_or(Fit::Fits);
    Report { fit, nodes, edges }
}

/// The node indexes with producers before consumers, then the nodes of cycles in graph order,
/// and which nodes are in or behind a cycle.
fn ordered(graph: &Graph, index: &HashMap<&str, usize>) -> (Vec<usize>, Vec<bool>) {
    let count = graph.nodes.len();
    let mut waiting = vec![0usize; count];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); count];
    for edge in &graph.edges {
        if let (Some(&from), Some(&to)) = (
            index.get(edge.from.node.as_str()),
            index.get(edge.to.node.as_str()),
        ) {
            waiting[to] += 1;
            next[from].push(to);
        }
    }
    let mut order: Vec<usize> = (0..count).filter(|at| waiting[*at] == 0).collect();
    let mut done = 0;
    while done < order.len() {
        for &to in &next[order[done]] {
            waiting[to] -= 1;
            if waiting[to] == 0 {
                order.push(to);
            }
        }
        done += 1;
    }
    let cyclic: Vec<bool> = waiting.iter().map(|left| *left > 0).collect();
    order.extend((0..count).filter(|at| cyclic[*at]));
    (order, cyclic)
}

/// The supply of an edge's output: resolved when its node is checked, else from its declaration.
fn supplied(
    graph: &Graph,
    tools: &[Definition],
    index: &HashMap<&str, usize>,
    outputs: &[Option<BTreeMap<String, Supply>>],
    from: &End,
) -> Option<Supply> {
    let at = *index.get(from.node.as_str())?;
    if let Some(resolved) = &outputs[at] {
        return resolved.get(&from.port).cloned();
    }
    let node = &graph.nodes[at];
    let tool = tools
        .iter()
        .find(|tool| tool.id == node.tool && tool.version == node.version)?;
    Supply::output(tool, &from.port, &BTreeMap::new(), false)
}

fn resolved(
    definition: &Definition,
    inputs: &BTreeMap<String, Supply>,
    scatter: bool,
) -> BTreeMap<String, Supply> {
    definition
        .outputs
        .iter()
        .filter_map(|output| {
            let supply = Supply::output(definition, &output.name, inputs, scatter)?;
            Some((output.name.clone(), supply))
        })
        .collect()
}

/// The finding of a required input without a connection. An input with a condition is required
/// only when the parameters meet it, and unknown while the node has no parameters yet.
fn unconnected(port: &InputPort, given: &Json, values: &Json) -> Option<Finding> {
    let fit = match &port.active_when {
        None => Fit::DoesNotFit,
        Some(_) if given.is_null() => Fit::Unknown,
        Some(condition) => match parameters::check(condition, values).first() {
            None => Fit::DoesNotFit,
            Some(finding) if finding.fit == Fit::Unknown => Fit::Unknown,
            Some(_) => return None,
        },
    };
    let reason = match fit {
        Fit::Unknown => format!(
            "the input {} is required for some parameter values, which are not known yet",
            port.name
        ),
        _ => format!("the required input {} is not connected", port.name),
    };
    Some(Finding::new(Aspect::Connection, fit, reason))
}

fn broken(reason: String) -> Check {
    Check {
        fit: Fit::DoesNotFit,
        scatter: false,
        format: FormatMatch::Unknown,
        findings: vec![Finding::new(Aspect::Connection, Fit::DoesNotFit, reason)],
    }
}

fn report(node: &Node, scatter: bool, findings: Vec<Finding>) -> NodeReport {
    NodeReport {
        id: node.id.clone(),
        fit: worst(&findings),
        scatter,
        findings,
    }
}
