//! nf-seqlab-progress 0.2.0: the functions that register the source files, stages and processes
//! of an nf-seqlab pipeline for its progress dashboard.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod registry;
#[expect(dead_code, reason = "the progress observer uses it")]
mod view;

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_sqldb::thrown;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "registerProgressInputs",
        "registerProgressStages",
        "withProgressIdentity",
    ],
    call: Some(call),
    ..Plugin::new("nf-seqlab-progress", Kind::Observer, State::Partial)
};

fn call(name: &str, arguments: Vec<Value>, _: &mut Context) -> Result<Value, Error> {
    let argument = |place: usize| arguments.get(place).cloned().unwrap_or(Value::Null);
    let done = match name {
        "registerProgressInputs" => registry::inputs(&argument(0)).map(|()| Value::Null),
        "registerProgressStages" => {
            registry::stages(&argument(0), &argument(1)).map(|()| Value::Null)
        }
        "withProgressIdentity" => registry::identity(&argument(0)),
        _ => {
            return Err(Error::Unsupported {
                message: format!("the function `{name}` of the plugin `nf-seqlab-progress`"),
                span: Default::default(),
            });
        }
    };
    done.map_err(thrown)
}
