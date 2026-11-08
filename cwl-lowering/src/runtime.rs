//! The `runtime` object that CWL expressions of a tool see, known when the tool lowers, and
//! the code of an expr that sees it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Dynamic, Expression, Part, RequirementClass, Version};
use serde_json::json;

use crate::Scope;

/// The output folder of a CWL tool in its container, its working folder, as in uwe's driver.
pub const OUTDIR: &str = "/var/spool/cwl";

/// Where uwe's CWL driver stages the input files of a tool in its container.
pub const STAGING: &str = "/var/lib/cwl/inputs";

/// The temporary folder of a CWL tool in its container.
pub const TMPDIR: &str = "/tmp";

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
    format!(
        "${{ var runtime = {runtime}; {TEXT} return {}; }}",
        value(code)
    )
}

/// The code of an expression whose parameter references fail on a missing field, as CWL
/// evaluates them without JavaScript, where JavaScript gives `undefined`.
pub fn strict(code: &Expression) -> String {
    if !(code.parts.iter()).any(|part| matches!(part, Part::Reference(_))) {
        return code.text.clone();
    }
    let parts: Vec<Part> = (code.parts.iter())
        .map(|part| match part {
            Part::Reference(path) => Part::Javascript(lookup(path)),
            other => other.clone(),
        })
        .collect();
    format!("${{ {GET} {TEXT} return {}; }}", value_of(&parts))
}

/// A function `get` that takes a field of an object or the length or an item of an array, and
/// fails on anything else.
const GET: &str = "var get = function(v, k) { \
    if (Array.isArray(v)) { if (k === \"length\") { return v.length; } \
    if (typeof k === \"number\" && k < v.length) { return v[k]; } } \
    else if (v !== null && typeof v === \"object\" && Object.prototype.hasOwnProperty.call(v, k)) \
    { return v[k]; } throw new Error(\"the parameter reference has no \" + k); };";

/// A parameter reference like `inputs.a['b'][0]` as calls of `get`.
fn lookup(path: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let end = path.find(|c: char| !word(c)).unwrap_or(path.len());
    let mut code = path[..end].to_owned();
    let mut rest = &path[end..];
    while !rest.is_empty() {
        let (key, after) = if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(|c: char| !word(c)).unwrap_or(tail.len());
            (
                serde_json::to_string(&tail[..end]).unwrap_or_default(),
                &tail[end..],
            )
        } else if let Some(tail) = rest.strip_prefix("['").or(rest.strip_prefix("[\"")) {
            let quote = rest.as_bytes()[1] as char;
            let end = tail.find(quote).unwrap_or(tail.len());
            let key = tail[..end].replace("\\'", "'").replace("\\\"", "\"");
            let after = tail.get(end + 2..).unwrap_or_default();
            (serde_json::to_string(&key).unwrap_or_default(), after)
        } else if let Some(tail) = rest.strip_prefix('[') {
            let end = tail.find(']').unwrap_or(tail.len());
            (
                tail[..end].to_owned(),
                tail.get(end + 1..).unwrap_or_default(),
            )
        } else {
            break;
        };
        code = format!("get({code}, {key})");
        rest = after;
    }
    code
}

/// The code of a list of expressions, like the globs of an output, as one array of their
/// values, where a value that is an array adds its items.
pub fn list(codes: &[Expression]) -> String {
    let values: Vec<String> = codes.iter().map(value).collect();
    format!("${{ {TEXT} return [].concat({}); }}", values.join(", "))
}

/// The code that sets the format of each File of `self`, a File or a list, to the value of a
/// format expression over that File as `self`, as CWL sets the format of an output.
pub fn formats(format: &Expression) -> String {
    format!(
        "${{ {TEXT} var format = function(self) {{ return {}; }}; \
         var set = function(file) {{ if (file !== null && typeof file === \"object\" \
         && file.class === \"File\") {{ file.format = format(file); }} return file; }}; \
         return Array.isArray(self) ? self.map(set) : set(self); }}",
        value(format)
    )
}

/// The function `text` that the code of text with parts uses.
const TEXT: &str =
    "var text = function(v) { return typeof v === \"string\" ? v : JSON.stringify(v); };";

/// The code of parts as one string, the text of each value joined.
pub fn joined(parts: &[Part]) -> String {
    format!("${{ {TEXT} return {}; }}", value_of(parts))
}

/// An expression as JavaScript code of its value.
fn value(code: &Expression) -> String {
    value_of(&code.parts)
}

fn value_of(parts: &[Part]) -> String {
    let part = |part: &Part| match part {
        Part::Text(text) => serde_json::to_string(text).unwrap_or_default(),
        Part::Function(body) => format!("(function() {{{body}}})()"),
        Part::Javascript(code) | Part::Reference(code) => format!("({code})"),
    };
    match parts {
        [] => "\"\"".into(),
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
    }
}
