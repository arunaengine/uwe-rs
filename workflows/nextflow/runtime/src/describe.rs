//! The descriptions of the native nodes of the dataflow network for the export.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::export::{Callback, Criteria, Kind, Order, callback, value};
use crate::files::CollectFile;
use crate::keyed::{Combine, Cross, GroupTuple, Join};
use crate::operator::{
    Buffer, Closing, Collate, Each, Factory, Gather, Gathering, Invoke, Opening, Single, Sort,
};
use crate::route::{Merge, Route};
use crate::split::{Split, Subscribe};

pub fn factory(node: &Factory) -> Result<Kind, String> {
    if let Some(failure) = &node.failure {
        return Err(format!("a factory that fails: {failure}"));
    }
    if let Some((factory, arguments)) = &node.call {
        let arguments = arguments.iter().map(value).collect::<Result<_, _>>()?;
        let factory = factory.to_string();
        return Ok(Kind::Source { factory, arguments });
    }
    let values = node.values.iter().map(value).collect::<Result<_, _>>()?;
    let close = node.close;
    Ok(Kind::Values { values, close })
}

pub fn single(node: &Single) -> Result<Kind, String> {
    Ok(match &node.kind {
        Each::Map(closure) => Kind::Map(Callback::of(closure)?),
        Each::Filter(criteria) => Kind::Filter(Criteria::of(criteria)?),
        Each::FlatMap(closure) => Kind::FlatMap(callback(closure)?),
        Each::Flatten => Kind::Flatten,
        Each::Cast(class) => Kind::Cast(class.to_string()),
        Each::View { closure, new_line } => Kind::View {
            callback: callback(closure)?,
            new_line: *new_line,
        },
        Each::Unique { closure, .. } => Kind::Unique(callback(closure)?),
        Each::Distinct { closure, .. } => Kind::Distinct(callback(closure)?),
        Each::Take(count) => Kind::Take(*count),
        Each::Until(closure) => Kind::Until(Callback::of(closure)?),
        Each::Transpose { by, remainder } => Kind::Transpose {
            by: by.clone(),
            remainder: *remainder,
        },
        Each::Scan { closure, running } => Kind::Scan {
            callback: Callback::of(closure)?,
            seed: value(running)?,
        },
    })
}

pub fn gathering(node: &Gathering) -> Result<Kind, String> {
    let criteria = |value: &Option<_>| value.as_ref().map(Criteria::of).transpose();
    Ok(match &node.kind {
        Gather::Collect {
            closure,
            flat,
            sort,
            sort_by,
        } => Kind::Collect {
            callback: callback(closure)?,
            flat: *flat,
            sort: match (sort, sort_by) {
                (false, _) => Order::None,
                (true, None) => Order::Natural,
                (true, Some(by)) => {
                    let by = nextflow_evaluator::value::Value::Closure(by.clone());
                    order(&Sort::new(Some(by), "collect").map_err(|e| e.to_string())?)?
                }
            },
        },
        Gather::ToList => Kind::ToList,
        Gather::IfEmpty(default) => Kind::IfEmpty(Criteria::of(default)?),
        Gather::First(found) => Kind::First(criteria(found)?),
        Gather::ToSortedList(closure) => Kind::ToSortedList(callback(closure)?),
        Gather::Count(found) => Kind::Count(criteria(found)?),
        Gather::Sum(closure) => Kind::Sum(callback(closure)?),
        Gather::Min(closure) => Kind::Min(callback(closure)?),
        Gather::Max(closure) => Kind::Max(callback(closure)?),
        Gather::Reduce(closure) => Kind::Reduce {
            callback: Callback::of(closure)?,
            seed: node.items.first().map(value).transpose()?,
        },
        Gather::TypedReduce(_) => return Err("the `reduce` of typed workflows".into()),
        Gather::Last => Kind::Last,
    })
}

pub fn order(sort: &Sort) -> Result<Order, String> {
    Ok(match sort {
        Sort::None => Order::None,
        Sort::Natural => Order::Natural,
        Sort::Key(closure) => Order::Key(Callback::of(closure)?),
        Sort::Compare(closure) => Order::Compare(Callback::of(closure)?),
        Sort::Hash(deep) => Order::Hash { deep: *deep },
    })
}

pub fn collate(node: &Collate) -> Kind {
    Kind::Collate {
        size: node.size,
        step: node.step,
        remainder: node.remainder,
    }
}

pub fn buffer(node: &Buffer) -> Result<Kind, String> {
    let (open, skip) = match &node.opening {
        Opening::Always => (None, 0),
        Opening::Skip { skip, .. } => (None, *skip),
        Opening::Criteria(criteria) => (Some(Criteria::of(criteria)?), 0),
    };
    let (close, size) = match &node.closing {
        Closing::Size { size, .. } => (None, *size),
        Closing::Criteria(criteria) => (Some(Criteria::of(criteria)?), 0),
    };
    let remainder = node.remainder;
    Ok(Kind::Buffer {
        open,
        close,
        size,
        skip,
        remainder,
    })
}

pub fn invoke(node: &Invoke) -> Result<Kind, String> {
    let arguments = (node.arguments.iter())
        .map(|argument| argument.as_ref().map(value).transpose())
        .collect::<Result<_, _>>()?;
    Ok(Kind::Invoke {
        name: node.name.to_string(),
        arguments,
        property: node.property,
    })
}

pub fn group_tuple(node: &GroupTuple) -> Result<Kind, String> {
    Ok(Kind::GroupTuple {
        by: node.by.clone(),
        size: node.size,
        remainder: node.remainder,
        sort: order(&node.sort)?,
    })
}

pub fn join(node: &Join) -> Kind {
    Kind::Join {
        by: node.by.clone(),
        remainder: node.remainder,
        duplicate_error: node.fail_on_duplicate,
        mismatch_error: node.fail_on_mismatch,
    }
}

pub fn combine(node: &Combine) -> Result<Kind, String> {
    // A right side that is no channel was taken whole when the node was made.
    let right = match node.done[1] {
        true => Some(node.seen[1].iter().map(value).collect::<Result<_, _>>()?),
        false => None,
    };
    let by = node.by.clone();
    Ok(Kind::Combine { by, right })
}

pub fn cross(node: &Cross) -> Result<Kind, String> {
    Ok(Kind::Cross(callback(&node.key)?))
}

pub fn route(node: &Route) -> Result<Kind, String> {
    Ok(Kind::Route {
        labels: node.labels.iter().map(ToString::to_string).collect(),
        callback: Callback::of(&node.criteria)?,
        all: node.all,
    })
}

pub fn merge(node: &Merge) -> Result<Kind, String> {
    Ok(Kind::Merge(callback(&node.closure)?))
}

pub fn collect_file(node: &CollectFile) -> Result<Kind, String> {
    Ok(Kind::CollectFile {
        callback: callback(&node.closure)?,
        name: node.name.clone(),
        directory: node.directory.uri(),
        new_line: node.new_line,
        sort: order(&node.sort)?,
        skip: node.skip,
        keep_header: node.keep_header,
        seed: node.seed.clone(),
    })
}

pub fn split(node: &Split) -> Result<Kind, String> {
    Ok(Kind::Split {
        splitter: node.splitter.clone(),
        elem: node.elem,
        each: callback(&node.each)?,
        decompress: node.decompress,
        count: node.total.is_some(),
        files: node.files.is_some(),
    })
}

pub fn subscribe(node: &Subscribe) -> Result<Kind, String> {
    Ok(Kind::Subscribe {
        next: callback(&node.on_next)?,
        complete: callback(&node.on_complete)?,
        error: callback(&node.on_error)?,
    })
}
