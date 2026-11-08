//! Load ops for the inputs whose files CWL reads before a process runs: the contents of Files
//! with `loadContents` and the listings of Directories with `loadListing`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{InputParameter, LoadListing, RequirementClass, Type as Cwl, short_name};
use wir::{Kind, Listing, Load, Port};

use crate::group::Group;
use crate::types::{Named, resolve};
use crate::{Scope, blank};

/// What CWL reads into an input before the process runs, or why a load op cannot read it.
pub(crate) fn of(
    input: &InputParameter,
    scope: &Scope,
    named: &Named,
) -> Result<Option<Load>, String> {
    let binding = input.binding.as_ref();
    let contents = input.load_contents == Some(true)
        || binding.is_some_and(|binding| binding.load_contents == Some(true));
    let required = scope.find(|class| match class {
        RequirementClass::LoadListing { load_listing } => *load_listing,
        _ => None,
    });
    let listing = match input.load_listing.or(required) {
        Some(LoadListing::ShallowListing) => Some(Listing::Shallow),
        Some(LoadListing::DeepListing) => Some(Listing::Deep),
        _ => None,
    };
    let listing = listing.filter(|_| folders(&input.value_type, named, 0));
    if fields(&input.value_type, named, 0) {
        let name = short_name(&input.id);
        return Err(format!(
            "loadContents or loadListing in a field of input {name}"
        ));
    }
    Ok((contents || listing.is_some()).then_some(Load { contents, listing }))
}

/// Whether a type can hold a Directory.
fn folders(r#type: &Cwl, named: &Named, depth: usize) -> bool {
    match resolve(r#type, named) {
        _ if depth > 32 => false,
        Cwl::Directory | Cwl::Any => true,
        Cwl::Array(array) => folders(&array.items, named, depth + 1),
        Cwl::Union(members) => (members.iter()).any(|member| folders(member, named, depth + 1)),
        Cwl::Record(record) => {
            (record.fields.iter()).any(|field| folders(&field.value_type, named, depth + 1))
        }
        _ => false,
    }
}

/// Whether a record in a type loads the contents or listing of a field.
fn fields(r#type: &Cwl, named: &Named, depth: usize) -> bool {
    match resolve(r#type, named) {
        _ if depth > 32 => false,
        Cwl::Array(array) => fields(&array.items, named, depth + 1),
        Cwl::Union(members) => (members.iter()).any(|member| fields(member, named, depth + 1)),
        Cwl::Record(record) => record.fields.iter().any(|field| {
            field.load_contents.is_some()
                || field.load_listing.is_some()
                || fields(&field.value_type, named, depth + 1)
        }),
        _ => false,
    }
}

/// The group with a load op first for each loaded input, which feeds every op that took it.
pub(crate) fn loaded(mut group: Group, loads: &[(Port, Load)]) -> Group {
    let main = group.main().path.clone();
    for (port, load) in loads.iter().rev() {
        let at = format!("{main}.load.{}", port.name);
        let takers = group.takers(&port.name);
        let mut op = blank(&at, Kind::Load(load.clone()), group.main().origin.clone());
        let out = Port {
            name: "out".into(),
            default: None,
            ..port.clone()
        };
        (op.inputs, op.outputs) = (vec![port.clone()], vec![out]);
        for (path, input) in takers {
            group
                .edges
                .push(crate::edge((Some(&at), "out"), (Some(&path), &input)));
        }
        group.ops.insert(0, op);
    }
    group
}
