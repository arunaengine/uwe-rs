//! The typed CWL v1.2 model, and how it is read from documents after Salad preprocessing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod binding;
mod check;
mod expression;
mod parameter;
mod process;
pub mod record;
mod requirement;
mod schema;
mod typecheck;
mod upgrade;
mod workflow;

pub use binding::{CommandLineBinding, LoadListing, OutputBinding};
pub use expression::{Dynamic, Expression, ExpressionKind, Part};
pub use parameter::{InputParameter, LinkMerge, OutputParameter, PickValue};
pub use process::{Argument, Class, CommandLineTool, ExpressionTool, Kind, Process, Version};
pub use requirement::{
    Dirent, Docker, EnvironmentDef, Listing, ListingEntry, Requirement, RequirementClass, Resource,
    SoftwarePackage, read_requirements,
};
pub use schema::{
    ArraySchema, EnumSchema, RecordField, RecordSchema, SecondaryFile, Type, short_name,
};
pub use workflow::{Run, Runs, ScatterMethod, Step, StepInput, Workflow};

pub(crate) use check::check_workflows;
pub(crate) use process::read_process;
