//! The execution graph of a recorded run, as the engine built it while the run went on, so that
//! the page draws the same graph as `iunctus graph`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use workflow_graph::{
    Edge, EdgeKind, ExecutionGraph, Language, Marker, MarkerKind, Node, Task, TaskId,
};

use crate::{Database, DatabaseError};

impl Database {
    /// Reads tasks, last attempt state, reuse, markers and edges; unknown runs return `None`.
    pub fn graph(&self, run: &str) -> Result<Option<ExecutionGraph>, DatabaseError> {
        let Some(record) = self.run(run)? else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT t.description, a.exit_code, a.reused IS NOT NULL, a.accepted
             FROM tasks t LEFT JOIN attempts a ON a.run = t.run AND a.task = t.id
                AND a.number = (SELECT MAX(number) FROM attempts WHERE run = t.run AND task = t.id)
             WHERE t.run = ?1 ORDER BY t.rowid",
        )?;
        let rows = statement
            .query_map([run], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })?
            .collect::<Result<Vec<(String, Option<i32>, Option<bool>, Option<bool>)>, _>>()?;
        let mut markers: HashMap<u64, Vec<Marker>> = HashMap::new();
        for marker in self.markers(run)? {
            let kind: MarkerKind = serde_json::from_value(marker.kind.into())?;
            let found = markers.entry(marker.task).or_default();
            found.push(Marker::new(kind, marker.reason));
        }
        let mut nodes = Vec::with_capacity(rows.len());
        for (description, exit_code, reused, accepted) in rows {
            let task: Task = serde_json::from_str(&description)?;
            nodes.push(Node {
                markers: markers.remove(&task.id.0).unwrap_or_default(),
                task,
                exit_code,
                accepted,
                reused: reused.unwrap_or(false),
            });
        }
        let edges = self
            .edges(run)?
            .into_iter()
            .map(|edge| Edge {
                from: TaskId(edge.source),
                to: TaskId(edge.target),
                kind: match edge.path {
                    Some(path) if edge.kind == "file" => EdgeKind::File { path },
                    _ => EdgeKind::Order,
                },
            })
            .collect();
        let language: Option<Language> = serde_json::from_value(record.language.into()).ok();
        let mut graph = ExecutionGraph {
            language,
            workflow: record.workflow,
            nodes,
            edges,
            ..ExecutionGraph::default()
        };
        if record.simulated {
            graph.simulate();
        }
        Ok(Some(graph))
    }
}
