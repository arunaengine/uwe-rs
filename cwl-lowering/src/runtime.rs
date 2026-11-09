//! The `runtime` object that CWL expressions of a tool see, known when the tool lowers, and
//! the code of an expr that sees it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_parser::model::{Dynamic, Expression, Part, RequirementClass, Version};
use serde_json::json;
use wir::Resources;

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
    let (cores, ram, tmpdir, outdir) = amounts(scope, version)?;
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

/// The resources of a tool as uwe's CWL driver gives them to its task, when no expression
/// sets them and the cores are whole.
pub fn resources(scope: &Scope, version: Version) -> Option<Resources> {
    let (cores, ram, tmpdir, outdir) = amounts(scope, version).ok()?;
    let mebibytes = |amount: f64| (amount.ceil() as u64).saturating_mul(1_048_576);
    (cores.fract() == 0.0).then(|| Resources {
        cpus: Some(cores as u32),
        memory: Some(mebibytes(ram)),
        disk: Some(mebibytes(tmpdir).saturating_add(mebibytes(outdir))),
        gpus: None,
    })
}

/// The cores, RAM, temporary and output folder sizes of a tool, in MiB, or why expressions
/// set them.
fn amounts(scope: &Scope, version: Version) -> Result<(f64, f64, f64, f64), String> {
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
    Ok((cores, ram, tmpdir, outdir))
}

/// The task input that holds the `runtime` object when expressions set the resources.
pub const VALUES: &str = "runtime.values";

/// The task input that holds the limits of the task when expressions set them.
pub const LIMITS: &str = "runtime.limits";

/// The code of the `runtime` object and of the task limits, by the inputs, when expressions
/// set the resources or the time limit, as uwe's CWL driver computes them.
pub fn computed(scope: &Scope, version: Version) -> Option<(String, String)> {
    let resource = scope.find(|class| match class {
        RequirementClass::Resource(resource) => Some(resource),
        _ => None,
    });
    let time = scope.find(|class| match class {
        RequirementClass::ToolTimeLimit { timelimit } => Some(timelimit),
        _ => None,
    });
    let amount = |setting: &Option<Dynamic<f64>>| match setting {
        Some(Dynamic::Value(value)) => value.to_string(),
        Some(Dynamic::Expression(code)) => value(code),
        None => "null".into(),
    };
    let pairs = resource.map(|resource| {
        [
            (&resource.cores_min, &resource.cores_max),
            (&resource.ram_min, &resource.ram_max),
            (&resource.tmpdir_min, &resource.tmpdir_max),
            (&resource.outdir_min, &resource.outdir_max),
        ]
    });
    let dynamic = |setting: &Option<Dynamic<f64>>| matches!(setting, Some(Dynamic::Expression(_)));
    let set = pairs
        .iter()
        .flatten()
        .any(|(min, max)| dynamic(min) || dynamic(max));
    // Cores that are not whole fit no task attribute.
    let whole = resources(scope, version).is_some();
    if whole && !set && !matches!(time, Some(Dynamic::Expression(_))) {
        return None;
    }
    let ram = match version {
        Version::V1_0 => 1024,
        _ => 256,
    };
    let picks: Vec<String> = match pairs {
        Some(pairs) => (pairs.iter().zip([1, ram, 1024, 1024]))
            .map(|((min, max), fallback)| {
                format!("pick({}, {}, {fallback})", amount(min), amount(max))
            })
            .collect(),
        None => vec!["1".into(), ram.to_string(), "1024".into(), "1024".into()],
    };
    let time = match time {
        Some(Dynamic::Value(seconds)) => seconds.to_string(),
        Some(Dynamic::Expression(code)) => value(code),
        None => "null".into(),
    };
    let base = format!(
        "var runtime = {{\"outdir\": \"{OUTDIR}\", \"tmpdir\": \"{TMPDIR}\"}}; {TEXT} {PICK} \
         var cores = {}, ram = {}, tmpdir = {}, outdir = {}; runtime.cores = Math.ceil(cores); \
         runtime.ram = Math.ceil(ram); runtime.outdirSize = Math.ceil(outdir); \
         runtime.tmpdirSize = Math.ceil(tmpdir); var time = {time}; \
         if (time !== null && time !== undefined && (time % 1 !== 0 || time < 0)) \
         {{ throw new Error(\"the timelimit is no whole number of 0 or more\"); }} \
         var mib = function(v) {{ return Math.ceil(v) * 1048576; }};",
        picks[0], picks[1], picks[2], picks[3]
    );
    let values = format!(
        "${{ {base} return {{\"outdir\": runtime.outdir, \"tmpdir\": runtime.tmpdir, \
         \"cores\": Math.ceil(cores), \"ram\": Math.ceil(ram), \
         \"outdirSize\": Math.ceil(outdir), \"tmpdirSize\": Math.ceil(tmpdir)}}; }}"
    );
    let limits = format!(
        "${{ {base} return {{\"cpus\": cores, \"memory\": mib(ram), \
         \"disk\": mib(tmpdir) + mib(outdir), \"time\": time > 0 ? time : null}}; }}"
    );
    Some((values, limits))
}

/// A function `pick` that takes the least amount, else the most, else the fallback, and
/// fails on an amount that is not finite, negative or over the most.
const PICK: &str = "var pick = function(min, max, fallback) { \
    var bad = function(v) { return typeof v !== \"number\" || !isFinite(v) || v < 0; }; \
    var given = function(v) { return v !== null && v !== undefined; }; \
    if ((given(min) && bad(min)) || (given(max) && bad(max)) || (given(min) && given(max) && min > max)) \
    { throw new Error(\"resource amounts must be finite, nonnegative and min <= max\"); } \
    return given(min) ? min : given(max) ? max : fallback; };";

/// The code of an expression that reads `runtime`, as a function that sees `runtime`, the
/// JSON object of the tool with the fields that the runner gives, like `exitCode`.
pub fn with(code: &Expression, runtime: &str) -> String {
    if !code.text.contains("runtime") {
        return code.text.clone();
    }
    format!(
        "${{ return (function(runtime) {{ {TEXT} return {}; }})((function(given) \
         {{ var made = {runtime}; for (var key in given) {{ made[key] = given[key]; }} \
         return made; }})(runtime)); }}",
        value(code)
    )
}

/// The text of an expression whose references that name no parameter, like `$(true)`, are
/// code; none when it has no such reference.
pub fn marked(code: &Expression) -> Option<String> {
    let mut text = code.text.clone();
    let mut changed = false;
    for part in &code.parts {
        let Part::Reference(path) = part else {
            continue;
        };
        let head: String = path
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !["inputs", "self", "runtime", "null"].contains(&head.as_str()) {
            text = text.replacen(&format!("$({path})"), &format!("$(({path}))"), 1);
            changed = true;
        }
    }
    changed.then_some(text)
}

/// The text of an expression without code whose references to fields of `runtime` read the
/// object, given as JSON, or the task input of it when `given`; none when that needs code, as
/// for a reference that keeps a number type when `typed`.
pub fn inlined(
    code: &Expression,
    object: Option<&str>,
    given: bool,
    typed: bool,
) -> Option<String> {
    let object: Option<serde_json::Value> = object.and_then(|text| serde_json::from_str(text).ok());
    let single = code.parts.len() == 1;
    let mut text = String::new();
    for part in &code.parts {
        match part {
            Part::Text(piece) if piece.contains("$(") || piece.contains("${") => return None,
            Part::Text(piece) => text.push_str(piece),
            Part::Reference(path) if path == "runtime" || path.starts_with("runtime[") => {
                return None;
            }
            Part::Reference(path) => match path.strip_prefix("runtime.") {
                None | Some("exitCode") => text.push_str(&format!("$({path})")),
                Some(field) if !field.chars().all(|c| c.is_alphanumeric() || c == '_') => {
                    return None;
                }
                Some(field) => match (&object, given) {
                    (Some(object), _) => match object.get(field)? {
                        serde_json::Value::String(value) => text.push_str(value),
                        _ if single && typed => return None,
                        value => text.push_str(&value.to_string()),
                    },
                    (None, true) => text.push_str(&format!("$(inputs[{VALUES:?}].{field})")),
                    (None, false) => return None,
                },
            },
            _ => return None,
        }
    }
    Some(text)
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
