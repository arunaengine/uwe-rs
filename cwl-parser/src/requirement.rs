//! Requirements and hints: the known classes as types, any other class kept as written.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::binding::{LoadListing, read_listing};
use crate::error::Error;
use crate::expression::{Dynamic, Expression};
use crate::node::{Entry, Location, Node, Value};
use crate::process::Version;
use crate::record::{self, Context, Record};
use crate::schema::{Type, read_type};

/// One entry of `requirements` or `hints`.
#[derive(Clone, Debug, PartialEq)]
pub struct Requirement {
    pub class: RequirementClass,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RequirementClass {
    InlineJavascript {
        expression_lib: Vec<String>,
    },
    SchemaDef {
        types: Vec<Type>,
    },
    LoadListing {
        load_listing: Option<LoadListing>,
    },
    Docker(Docker),
    Software {
        packages: Vec<SoftwarePackage>,
    },
    InitialWorkDir {
        listing: Listing,
    },
    EnvVar {
        variables: Vec<EnvironmentDef>,
    },
    ShellCommand,
    Resource(Box<Resource>),
    WorkReuse {
        enable_reuse: Dynamic<bool>,
    },
    NetworkAccess {
        network_access: Dynamic<bool>,
    },
    InplaceUpdate {
        inplace_update: bool,
    },
    ToolTimeLimit {
        timelimit: Dynamic<i64>,
    },
    SubworkflowFeature,
    ScatterFeature,
    MultipleInputFeature,
    StepInputExpression,
    /// A class this model does not know, such as a vendor extension, kept as written.
    Other {
        class: String,
        value: Node,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Docker {
    pub docker_pull: Option<String>,
    pub docker_load: Option<String>,
    pub docker_file: Option<String>,
    pub docker_import: Option<String>,
    pub docker_image_id: Option<String>,
    pub docker_output_directory: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoftwarePackage {
    pub package: String,
    pub version: Vec<String>,
    pub specs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentDef {
    pub env_name: String,
    pub env_value: Expression,
}

/// Amounts of cores, and of memory and disk in mebibytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    pub cores_min: Option<Dynamic<f64>>,
    pub cores_max: Option<Dynamic<f64>>,
    pub ram_min: Option<Dynamic<f64>>,
    pub ram_max: Option<Dynamic<f64>>,
    pub tmpdir_min: Option<Dynamic<f64>>,
    pub tmpdir_max: Option<Dynamic<f64>>,
    pub outdir_min: Option<Dynamic<f64>>,
    pub outdir_max: Option<Dynamic<f64>>,
}

/// The `listing` of an `InitialWorkDirRequirement`.
#[derive(Clone, Debug, PartialEq)]
pub enum Listing {
    Expression(Expression),
    Entries(Vec<ListingEntry>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ListingEntry {
    Dirent(Dirent),
    Expression(Expression),
    /// A `File` or `Directory` object, or a list of them, as written.
    Object(Node),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dirent {
    pub entryname: Option<Expression>,
    pub entry: Expression,
    pub writable: bool,
}

/// Classes added in v1.1, which a v1.0 document may not list as requirements.
const NEWER_CLASSES: [&str; 5] = [
    "LoadListingRequirement",
    "WorkReuse",
    "NetworkAccess",
    "InplaceUpdateRequirement",
    "ToolTimeLimit",
];

/// Extension classes of v1.0 that v1.1 made standard, and their standard names.
pub const RENAMED_CLASSES: [(&str, &str); 6] = [
    ("http://commonwl.org/cwltool#WorkReuse", "WorkReuse"),
    ("http://arvados.org/cwl#ReuseRequirement", "WorkReuse"),
    ("http://commonwl.org/cwltool#TimeLimit", "ToolTimeLimit"),
    ("http://commonwl.org/cwltool#NetworkAccess", "NetworkAccess"),
    (
        "http://commonwl.org/cwltool#InplaceUpdateRequirement",
        "InplaceUpdateRequirement",
    ),
    (
        "http://commonwl.org/cwltool#LoadListingRequirement",
        "LoadListingRequirement",
    ),
];

/// The standard name of a requirement class in a document of the given version.
pub fn class_name(class: &str, version: Version) -> &str {
    if version != Version::V1_0 {
        return class;
    }
    let renamed = RENAMED_CLASSES.iter().find(|(old, _)| *old == class);
    renamed.map_or(class, |(_, new)| new)
}

/// Reads the entries of `requirements` or `hints`; Salad has already turned maps into lists.
pub fn read_requirements(
    node: Option<&Node>,
    context: &Context,
    hints: bool,
) -> Result<Vec<Requirement>, Error> {
    let Some(node) = node else {
        return Ok(Vec::new());
    };
    record::list(node)?
        .iter()
        .map(|entry| read_requirement(entry, context, hints))
        .collect()
}

pub fn read_requirement(node: &Node, context: &Context, hint: bool) -> Result<Requirement, Error> {
    let what = if hint { "hint" } else { "requirement" };
    let mut record = Record::new(node, what)?;
    let class = record::text(record.required("class")?)?;
    if !hint && context.version == Version::V1_0 && NEWER_CLASSES.contains(&class.as_str()) {
        context.require(Version::V1_1, &format!("`{class}`"), &node.location)?;
    }
    let class = match class_name(&class, context.version) {
        "InlineJavascriptRequirement" => RequirementClass::InlineJavascript {
            expression_lib: record
                .optional("expressionLib")
                .map(record::texts)
                .transpose()?
                .unwrap_or_default(),
        },
        "SchemaDefRequirement" => {
            let mut types = Vec::new();
            for item in record::list(record.required("types")?)? {
                types.push(read_type(item, context, false)?);
            }
            RequirementClass::SchemaDef { types }
        }
        "LoadListingRequirement" => RequirementClass::LoadListing {
            load_listing: record
                .optional("loadListing")
                .map(read_listing)
                .transpose()?,
        },
        "DockerRequirement" => RequirementClass::Docker(read_docker(&mut record)?),
        "SoftwareRequirement" => RequirementClass::Software {
            packages: read_packages(record.required("packages")?)?,
        },
        "InitialWorkDirRequirement" => RequirementClass::InitialWorkDir {
            listing: read_listing_entries(record.required("listing")?, context)?,
        },
        "EnvVarRequirement" => RequirementClass::EnvVar {
            variables: read_variables(record.required("envDef")?, context)?,
        },
        "ShellCommandRequirement" => RequirementClass::ShellCommand,
        "ResourceRequirement" => {
            RequirementClass::Resource(Box::new(read_resource(&mut record, context)?))
        }
        "WorkReuse" => RequirementClass::WorkReuse {
            enable_reuse: match record.optional("enableReuse") {
                Some(node) => dynamic_bool(node, context)?,
                None => Dynamic::Value(true),
            },
        },
        "NetworkAccess" => RequirementClass::NetworkAccess {
            network_access: dynamic_bool(record.required("networkAccess")?, context)?,
        },
        "InplaceUpdateRequirement" => RequirementClass::InplaceUpdate {
            inplace_update: record::boolean(record.required("inplaceUpdate")?)?,
        },
        "ToolTimeLimit" => {
            let node = record.required("timelimit")?;
            let timelimit = match node.value {
                Value::Integer(seconds) => Dynamic::Value(seconds),
                _ => Dynamic::Expression(context.code(node)?),
            };
            RequirementClass::ToolTimeLimit { timelimit }
        }
        "SubworkflowFeatureRequirement" => RequirementClass::SubworkflowFeature,
        "ScatterFeatureRequirement" => RequirementClass::ScatterFeature,
        "MultipleInputFeatureRequirement" => RequirementClass::MultipleInputFeature,
        "StepInputExpressionRequirement" => RequirementClass::StepInputExpression,
        _ => {
            return Ok(Requirement {
                class: RequirementClass::Other {
                    class,
                    value: node.clone(),
                },
                extensions: Vec::new(),
                location: node.location.clone(),
            });
        }
    };
    let extensions = record.finish()?;
    Ok(Requirement {
        class,
        extensions,
        location: node.location.clone(),
    })
}

fn dynamic_bool(node: &Node, context: &Context) -> Result<Dynamic<bool>, Error> {
    match node.value {
        Value::Bool(value) => Ok(Dynamic::Value(value)),
        _ => Ok(Dynamic::Expression(context.code(node)?)),
    }
}

fn read_docker(record: &mut Record<'_>) -> Result<Docker, Error> {
    let mut field = |name| record.optional(name).map(record::text).transpose();
    Ok(Docker {
        docker_pull: field("dockerPull")?,
        docker_load: field("dockerLoad")?,
        docker_file: field("dockerFile")?,
        docker_import: field("dockerImport")?,
        docker_image_id: field("dockerImageId")?,
        docker_output_directory: field("dockerOutputDirectory")?,
    })
}

fn read_packages(node: &Node) -> Result<Vec<SoftwarePackage>, Error> {
    let mut packages = Vec::new();
    for item in record::list(node)? {
        let mut record = Record::new(item, "software package")?;
        let package = SoftwarePackage {
            package: record::text(record.required("package")?)?,
            version: record
                .optional("version")
                .map(record::texts)
                .transpose()?
                .unwrap_or_default(),
            specs: record
                .optional("specs")
                .map(record::texts)
                .transpose()?
                .unwrap_or_default(),
        };
        record.finish()?;
        packages.push(package);
    }
    Ok(packages)
}

fn read_variables(node: &Node, context: &Context) -> Result<Vec<EnvironmentDef>, Error> {
    let mut variables = Vec::new();
    for item in record::list(node)? {
        let mut record = Record::new(item, "environment variable")?;
        let variable = EnvironmentDef {
            env_name: record::text(record.required("envName")?)?,
            env_value: context.expression(record.required("envValue")?)?,
        };
        record.finish()?;
        variables.push(variable);
    }
    Ok(variables)
}

/// Reads the eight resource fields; their allowed forms differ between versions.
fn read_resource(record: &mut Record<'_>, context: &Context) -> Result<Resource, Error> {
    let mut field = |name: &str| -> Result<Option<Dynamic<f64>>, Error> {
        let Some(node) = record.optional(name) else {
            return Ok(None);
        };
        let amount = match node.value {
            Value::Integer(amount) => Dynamic::Value(amount as f64),
            Value::Float(amount) => {
                context.require(
                    Version::V1_2,
                    "a fractional resource amount",
                    &node.location,
                )?;
                Dynamic::Value(amount)
            }
            _ if context.version == Version::V1_0 => Dynamic::Expression(context.expression(node)?),
            _ => Dynamic::Expression(context.code(node)?),
        };
        Ok(Some(amount))
    };
    Ok(Resource {
        cores_min: field("coresMin")?,
        cores_max: field("coresMax")?,
        ram_min: field("ramMin")?,
        ram_max: field("ramMax")?,
        tmpdir_min: field("tmpdirMin")?,
        tmpdir_max: field("tmpdirMax")?,
        outdir_min: field("outdirMin")?,
        outdir_max: field("outdirMax")?,
    })
}

fn read_listing_entries(node: &Node, context: &Context) -> Result<Listing, Error> {
    let Value::List(items) = &node.value else {
        let expression = if context.version == Version::V1_0 {
            context.expression(node)?
        } else {
            context.code(node)?
        };
        return Ok(Listing::Expression(expression));
    };
    let mut entries = Vec::new();
    for item in items {
        let entry = match &item.value {
            Value::Null => continue,
            Value::String(_) if context.version == Version::V1_0 => {
                ListingEntry::Expression(context.expression(item)?)
            }
            Value::String(_) => ListingEntry::Expression(context.code(item)?),
            Value::List(_) => ListingEntry::Object(item.clone()),
            Value::Map(_) if item.get("class").is_some() => ListingEntry::Object(item.clone()),
            _ => ListingEntry::Dirent(read_dirent(item, context)?),
        };
        entries.push(entry);
    }
    Ok(Listing::Entries(entries))
}

fn read_dirent(node: &Node, context: &Context) -> Result<Dirent, Error> {
    let mut record = Record::new(node, "Dirent")?;
    let dirent = Dirent {
        entryname: record
            .optional("entryname")
            .map(|node| context.expression(node))
            .transpose()?,
        entry: context.expression(record.required("entry")?)?,
        writable: record
            .optional("writable")
            .map(record::boolean)
            .transpose()?
            .unwrap_or(false),
    };
    record.finish()?;
    Ok(dirent)
}
