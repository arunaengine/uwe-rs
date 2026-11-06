//! nf-fl86 0.6.4: `getAWSSecret` from AWS Secrets Manager and `call_seqera_action`, which
//! launches a Seqera Platform Action.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod secrets;
mod seqera;

use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_sqldb::thrown;
use secrets::Access;

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["getAWSSecret", "call_seqera_action"],
    call: Some(call),
    scopes: &["notion", "seqera_action"],
    ..Plugin::new("nf-fl86", Kind::Observer, State::Partial)
};

/// The Seqera Platform API when the call or the config names none.
const PLATFORM: &str = "https://api.cloud.seqera.io";

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let text = |place: usize| match arguments.get(place) {
        None | Some(Value::Null) => String::new(),
        Some(value) => value.to_string(),
    };
    match name {
        "getAWSSecret" => {
            let access = Access::of(context.config, &|name| context.host.environment(name));
            let region = Some(text(1)).filter(|region| !region.is_empty());
            let found = secrets::secret(&text(0), region.as_deref(), &access);
            found.map(Value::string).map_err(thrown)
        }
        "call_seqera_action" => {
            let params = crate::nf_schema::json(arguments.get(2).unwrap_or(&Value::Null));
            let base = Some(text(5)).filter(|base| !base.is_empty());
            let base = base.unwrap_or(PLATFORM.to_owned());
            if text(0).trim().is_empty() {
                tracing::warn!("Seqera action launch was skipped because the token is missing");
                return Ok(Value::map(Vec::new()));
            }
            match seqera::launch(&text(0), &text(1), &params, &text(3), &text(4), &base) {
                Ok(answer) => Ok(parse_json(&answer.to_string()).unwrap_or(Value::Null)),
                Err(reason) => {
                    tracing::warn!("Seqera action launch was not completed: {reason}");
                    Ok(Value::map(Vec::new()))
                }
            }
        }
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-fl86`"),
            span: Default::default(),
        }),
    }
}
