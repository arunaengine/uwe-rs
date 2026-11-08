//! The `runtime` object that CWL expressions of a tool see, known when the tool lowers, and
//! the code of an expr that sees it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Dynamic, Expression, Part, RequirementClass, Version};
use serde_json::json;

use crate::Scope;

/// The output folder of a CWL tool in its container, its working folder, as in uwe's driver.
pub const OUTDIR: &str = "/var/spool/cwl";

/// The temporary folder of a CWL tool in its container.
const TMPDIR: &str = "/tmp";

/// The `runtime` object of a tool as JSON, as uwe's CWL driver makes it, or why it is not
/// known before the tool runs.
pub fn object(scope: &Scope, version: Version) -> Result<String, String> {
    let resource = scope.find(|class| match class {
        RequirementClass::Resource(resource) => Some(resource),
        _ => None,
    });
    let ram = match version {
        Version::V1_0 => 1024.0,
        _ => 256.0,
    };
    let (mut cores, mut ram, mut tmpdir, mut outdir) = (1.0, ram, 1024.0, 1024.0);
    if let Some(resource) = resource {
        let amount = |setting: &Option<Dynamic<f64>>| match setting {
            Some(Dynamic::Value(value)) => Ok(Some(*value)),
            Some(Dynamic::Expression(_)) => Err("a resource set by an expression".to_owned()),
            None => Ok(None),
        };
        let pick = |min: &Option<Dynamic<f64>>, max: &Option<Dynamic<f64>>, default: f64| {
            Ok::<_, String>(amount(min)?.or(amount(max)?).unwrap_or(default))
        };
        cores = pick(&resource.cores_min, &resource.cores_max, cores)?;
        ram = pick(&resource.ram_min, &resource.ram_max, ram)?;
        tmpdir = pick(&resource.tmpdir_min, &resource.tmpdir_max, tmpdir)?;
        outdir = pick(&resource.outdir_min, &resource.outdir_max, outdir)?;
    }
    let whole = |amount: f64| amount.ceil() as i64;
    let object = json!({
        "outdir": OUTDIR,
        "tmpdir": TMPDIR,
        "cores": whole(cores),
        "ram": whole(ram),
        "outdirSize": whole(outdir),
        "tmpdirSize": whole(tmpdir),
    });
    Ok(object.to_string())
}

/// The code of an expression that reads `runtime`, as a function that sees `runtime`, the
/// JSON object of the tool, since an expr sees an empty one.
pub fn with(code: &Expression, runtime: &str) -> String {
    if !code.text.contains("runtime") {
        return code.text.clone();
    }
    let part = |part: &Part| match part {
        Part::Text(text) => serde_json::to_string(text).unwrap_or_default(),
        Part::Function(body) => format!("(function() {{{body}}})()"),
        Part::Javascript(code) | Part::Reference(code) => format!("({code})"),
    };
    let value = match code.parts.as_slice() {
        [one] => part(one),
        parts => {
            // Text with parts becomes a string, with values other than strings as JSON.
            let texts: Vec<String> = (parts.iter())
                .map(|item| match item {
                    Part::Text(_) => part(item),
                    _ => format!("text({})", part(item)),
                })
                .collect();
            format!("[{}].join(\"\")", texts.join(", "))
        }
    };
    format!(
        "${{ var runtime = {runtime}; \
         var text = function(v) {{ return typeof v === \"string\" ? v : JSON.stringify(v); }}; \
         return {value}; }}"
    )
}
