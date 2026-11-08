//! The merge, flatten and select ops of a CWL link with several sources, `linkMerge` or
//! `pickValue`, in the order in which uwe's CWL driver applies them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{LinkMerge, PickValue};
use wir::reserved::{Flatten, Merge, Pick, Select};
use wir::{ArrayType, Edge, Endpoint, Flow, Kind, Op, Port, Type};

use crate::blank;
use crate::workflow::end;

/// The ops that join the sources of one link, and the end that gives its value.
pub(crate) struct Link {
    pub ops: Vec<Op>,
    pub edges: Vec<Edge>,
    pub from: Endpoint,
}

/// The link of `sources`, ends with their types, through ops at `<path>.merge`,
/// `<path>.flatten` and `<path>.select`. Several sources merge nested unless `merge` says how.
pub(crate) fn join(
    path: &str,
    sources: &[(Endpoint, Type)],
    merge: Option<LinkMerge>,
    pick: Option<PickValue>,
) -> Result<Link, String> {
    let merge = merge.or((sources.len() > 1).then_some(LinkMerge::MergeNested));
    let (mut link, r#type) = match (merge, sources) {
        (None, [(from, r#type)]) => (start(from.clone()), r#type.clone()),
        (None, _) => return Err("a link without one source".into()),
        (Some(mode), _) => merged(path, sources, mode),
    };
    if let Some(pick) = pick {
        // pickValue picks among the items of an array and leaves any other value as it is.
        let items = match &r#type {
            Type::Array(array) => (*array.items).clone(),
            Type::Dynamic => return Err("pickValue of a value of unknown type".into()),
            _ => return Ok(link),
        };
        let item = match items {
            Type::Optional(inner) => *inner,
            other => other,
        };
        let (rule, output) = match pick {
            PickValue::FirstNonNull => (Pick::First, item),
            PickValue::OnlyNonNull => (Pick::Only, item),
            PickValue::AllNonNull => (Pick::All, array(item)),
        };
        let kind = Kind::Select(Select { rule });
        then(&mut link, &format!("{path}.select"), kind, r#type, output);
    }
    Ok(link)
}

/// The merge of the sources, flattened for `merge_flattened`, and the type it gives.
fn merged(path: &str, sources: &[(Endpoint, Type)], mode: LinkMerge) -> (Link, Type) {
    let first = sources.first().map(|(_, r#type)| r#type);
    let same = sources.iter().all(|(_, r#type)| Some(r#type) == first);
    let items = first.filter(|_| same).cloned().unwrap_or(Type::Dynamic);
    let at = format!("{path}.merge");
    let mut op = blank(&at, Kind::Merge(Merge {}), None);
    let mut link = start(end(Some(&at), "out"));
    for (index, (from, r#type)) in sources.iter().enumerate() {
        op.inputs.push(port(&index.to_string(), r#type.clone()));
        link.edges.push(Edge {
            from: from.clone(),
            to: end(Some(&at), &index.to_string()),
        });
    }
    op.outputs.push(port("out", array(items.clone())));
    link.ops.push(op);
    if mode == LinkMerge::MergeNested {
        return (link, array(items));
    }
    let inner = match &items {
        Type::Array(inner) => (*inner.items).clone(),
        other => other.clone(),
    };
    let kind = Kind::Flatten(Flatten {});
    then(
        &mut link,
        &format!("{path}.flatten"),
        kind,
        array(items),
        array(inner.clone()),
    );
    (link, array(inner))
}

/// Adds the op `at` of one input `in` and one output `out` after the end of the link.
fn then(link: &mut Link, at: &str, kind: Kind, input: Type, output: Type) {
    let mut op = blank(at, kind, None);
    op.inputs.push(port("in", input));
    op.outputs.push(port("out", output));
    link.edges.push(Edge {
        from: link.from.clone(),
        to: end(Some(at), "in"),
    });
    link.from = end(Some(at), "out");
    link.ops.push(op);
}

fn start(from: Endpoint) -> Link {
    Link {
        ops: Vec::new(),
        edges: Vec::new(),
        from,
    }
}

fn array(items: Type) -> Type {
    Type::Array(ArrayType {
        items: Box::new(items),
        nonempty: false,
    })
}

fn port(name: &str, r#type: Type) -> Port {
    Port {
        name: name.to_owned(),
        r#type,
        flow: Flow::Value,
        default: None,
    }
}
