//! Argo texts that wir computes: argo expr ops, which the Argo runtime of uwe substitutes as its
//! native run does, with the values of the tags they read as inputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::{Node, Position, Value as Yaml};
use wir::{Edge, Endpoint, Expr, ExprForm, ExprLanguage, Kind, Op, Origin, Type};

use crate::tags::{Tag, read_tags};
use crate::{SOURCE, array, edge, end, op, port, text};

/// The output of every argo expr op.
pub(crate) const VALUE: &str = "value";

/// The argo expr op at `path` that gives `form` of `code`, and the edges to its inputs: each
/// tag it reads that `source` finds a value for. Other tags come from the workflow globals.
pub(crate) fn text_op(
    path: &str,
    code: &str,
    form: ExprForm,
    source: impl Fn(&Tag) -> Option<(Endpoint, Type)>,
) -> Result<(Op, Vec<Edge>), String> {
    let tags = read_tags(&text_node(code))?;
    let expr = Expr {
        language: ExprLanguage::Argo,
        code: code.to_owned(),
        library: Vec::new(),
        scope: None,
        captures: Default::default(),
        form: (form != ExprForm::Text).then_some(form),
    };
    let mut made = op(path, Kind::Expr(expr));
    made.origin = Some(Origin {
        source: SOURCE.into(),
        at: None,
    });
    let mut edges = Vec::new();
    for tag in tags {
        let Some((from, r#type)) = source(&tag) else {
            continue;
        };
        let name = tag.name();
        made.inputs.push(port(name.clone(), r#type));
        edges.push(edge(from, end(Some(path), &name)));
    }
    let output = match form {
        ExprForm::Text => text(),
        ExprForm::Condition => Type::Bool,
        ExprForm::Items => array(Type::Dynamic),
        ExprForm::Labels => array(text()),
    };
    made.outputs.push(port(VALUE, output));
    Ok((made, edges))
}

/// A YAML node of one text, to read its tags.
pub(crate) fn text_node(text: &str) -> Node {
    Node::new(Yaml::String(text.to_owned()), Position::default())
}
