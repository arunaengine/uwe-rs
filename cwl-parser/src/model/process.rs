//! Processes: command line tools, expression tools, workflows and operations.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::error::Error;
use crate::model::binding::{CommandLineBinding, read_binding};
use crate::model::expression::Expression;
use crate::model::parameter::{InputParameter, OutputParameter, read_input, read_output_parameter};
use crate::model::record::{self, Context, Record};
use crate::model::requirement::{Requirement, read_requirements};
use crate::model::schema::Type;
use crate::model::upgrade::upgrade;
use crate::model::workflow::{Runs, Workflow, read_workflow};
use crate::node::{Entry, Location, Node, Value};

/// The CWL versions that load, in the order of cwltool's update chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    V1_0,
    V1_1Dev1,
    V1_1,
    V1_2Dev1,
    V1_2Dev2,
    V1_2Dev3,
    V1_2Dev4,
    V1_2Dev5,
    V1_2,
}

const VERSIONS: [(&str, Version); 9] = [
    ("v1.0", Version::V1_0),
    ("v1.1.0-dev1", Version::V1_1Dev1),
    ("v1.1", Version::V1_1),
    ("v1.2.0-dev1", Version::V1_2Dev1),
    ("v1.2.0-dev2", Version::V1_2Dev2),
    ("v1.2.0-dev3", Version::V1_2Dev3),
    ("v1.2.0-dev4", Version::V1_2Dev4),
    ("v1.2.0-dev5", Version::V1_2Dev5),
    ("v1.2", Version::V1_2),
];

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let found = VERSIONS.iter().find(|(name, _)| *name == text);
        found.map(|(_, version)| *version)
    }

    /// Whether backslashes in expressions follow the rule before v1.2.0-dev4.
    pub fn old_escapes(self) -> bool {
        self <= Version::V1_2Dev3
    }

    /// The release a development version led to; features are checked against it.
    pub fn release(self) -> Self {
        match self {
            Version::V1_1Dev1 => Version::V1_1,
            Version::V1_2Dev1
            | Version::V1_2Dev2
            | Version::V1_2Dev3
            | Version::V1_2Dev4
            | Version::V1_2Dev5 => Version::V1_2,
            release => release,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = VERSIONS.iter().find(|(_, version)| version == self);
        formatter.write_str(name.map_or("", |(name, _)| name))
    }
}

/// A process of any class, after the upgrade to the v1.2 model.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    /// The resolved identifier, such as `file:///work/tool.cwl` or `file:///work/wf.cwl#main`.
    pub id: String,
    /// The `cwlVersion` of the document it came from, before the upgrade.
    pub version: Version,
    pub label: Option<String>,
    pub doc: Vec<String>,
    pub inputs: Vec<InputParameter>,
    pub outputs: Vec<OutputParameter>,
    pub requirements: Vec<Requirement>,
    pub hints: Vec<Requirement>,
    pub intent: Vec<String>,
    pub class: Class,
    pub extensions: Vec<Entry>,
    pub location: Location,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Class {
    CommandLineTool(Box<CommandLineTool>),
    ExpressionTool(ExpressionTool),
    Workflow(Workflow),
    /// An abstract step that cannot run.
    Operation,
}

/// The class of a process without its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    CommandLineTool,
    ExpressionTool,
    Workflow,
    Operation,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandLineTool {
    pub base_command: Vec<String>,
    pub arguments: Vec<Argument>,
    pub stdin: Option<Expression>,
    pub stdout: Option<Expression>,
    pub stderr: Option<Expression>,
    pub success_codes: Vec<i64>,
    pub temporary_fail_codes: Vec<i64>,
    pub permanent_fail_codes: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Argument {
    Text(Expression),
    Binding(CommandLineBinding),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExpressionTool {
    pub expression: Expression,
}

impl Process {
    pub fn kind(&self) -> Kind {
        match self.class {
            Class::CommandLineTool(_) => Kind::CommandLineTool,
            Class::ExpressionTool(_) => Kind::ExpressionTool,
            Class::Workflow(_) => Kind::Workflow,
            Class::Operation => Kind::Operation,
        }
    }
}

/// Reads a resolved process; `fallback` becomes its identifier when it has none.
pub fn read_process(
    node: &Node,
    context: &Context,
    runs: &mut dyn Runs,
    fallback: &str,
) -> Result<Process, Error> {
    let mut record = Record::new(node, "process")?;
    let class_node = record.required("class")?;
    let kind = match class_node.as_str() {
        Some("CommandLineTool") => Kind::CommandLineTool,
        Some("ExpressionTool") => Kind::ExpressionTool,
        Some("Workflow") => Kind::Workflow,
        Some("Operation") => {
            context.require(Version::V1_2, "the Operation class", &class_node.location)?;
            Kind::Operation
        }
        _ => {
            let message = "`class` must be CommandLineTool, ExpressionTool, Workflow or Operation";
            return Err(Error::new(&class_node.location, message));
        }
    };
    let id = match record.optional("id") {
        Some(id) => record::text(id)?,
        None => fallback.to_owned(),
    };
    record.optional("cwlVersion");
    for name in ["requirements", "hints"] {
        if node
            .get(name)
            .is_some_and(|value| value.value == Value::Null)
        {
            let message = format!("`{name}` must be a list or a map, not empty");
            return Err(Error::new(&node.location, message));
        }
    }
    let label = record.optional("label").map(record::text).transpose()?;
    let doc = record::documentation(&mut record)?;
    let mut inputs = Vec::new();
    for input in record::list(record.required("inputs")?)? {
        inputs.push(read_input(input, context, kind)?);
    }
    let mut outputs = Vec::new();
    for output in record::list(record.required("outputs")?)? {
        outputs.push(read_output_parameter(output, context, kind)?);
    }
    let requirements = read_requirements(record.optional("requirements"), context, false)?;
    let hints = read_requirements(record.optional("hints"), context, true)?;
    let intent = record
        .since("intent", Version::V1_2, context)?
        .map(record::texts)
        .transpose()?
        .unwrap_or_default();
    let class = match kind {
        Kind::CommandLineTool => {
            Class::CommandLineTool(Box::new(read_tool(&mut record, context, &inputs)?))
        }
        Kind::ExpressionTool => {
            let node = record.required("expression")?;
            let expression = if context.version == Version::V1_0 {
                context.expression(node)?
            } else {
                context.code(node)?
            };
            Class::ExpressionTool(ExpressionTool { expression })
        }
        Kind::Workflow => Class::Workflow(read_workflow(&mut record, context, runs)?),
        Kind::Operation => Class::Operation,
    };
    let extensions = record.finish()?;
    let mut process = Process {
        id,
        version: context.version,
        label,
        doc,
        inputs,
        outputs,
        requirements,
        hints,
        intent,
        class,
        extensions,
        location: node.location.clone(),
    };
    upgrade(&mut process);
    Ok(process)
}

fn read_tool(
    record: &mut Record<'_>,
    context: &Context,
    inputs: &[InputParameter],
) -> Result<CommandLineTool, Error> {
    let base_command = record
        .optional("baseCommand")
        .map(record::texts)
        .transpose()?;
    let mut arguments = Vec::new();
    for argument in record
        .optional("arguments")
        .map(record::list)
        .transpose()?
        .unwrap_or(&[])
    {
        arguments.push(match argument.value {
            Value::String(_) => Argument::Text(context.expression(argument)?),
            _ => Argument::Binding(read_binding(argument, context)?),
        });
    }
    let stream = |record: &mut Record<'_>, name| -> Result<Option<Expression>, Error> {
        let node = record.optional(name);
        node.map(|node| context.expression(node)).transpose()
    };
    let tool = CommandLineTool {
        base_command: base_command.unwrap_or_default(),
        arguments,
        stdin: stream(record, "stdin")?,
        stdout: stream(record, "stdout")?,
        stderr: stream(record, "stderr")?,
        success_codes: codes(record, "successCodes")?,
        temporary_fail_codes: codes(record, "temporaryFailCodes")?,
        permanent_fail_codes: codes(record, "permanentFailCodes")?,
    };
    for input in inputs
        .iter()
        .filter(|input| input.value_type == Type::Stdin)
    {
        if input.binding.is_some() || tool.stdin.is_some() {
            let message = "an input of type stdin cannot have an inputBinding or a `stdin` field";
            return Err(Error::new(&input.location, message));
        }
    }
    Ok(tool)
}

fn codes(record: &mut Record<'_>, name: &str) -> Result<Vec<i64>, Error> {
    let Some(node) = record.optional(name) else {
        return Ok(Vec::new());
    };
    record::list(node)?.iter().map(record::integer).collect()
}
