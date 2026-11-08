//! WDL callbacks: an expression over named parameters and captures, run by the WDL
//! evaluator of uwe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Map, Value as Json};
use wdl_runtime::Value;
use wdl_runtime::expression::{Environment, Error, evaluate};

/// The names a WDL expression sees.
struct Names(HashMap<String, Value>);

impl Environment for Names {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.0.get(name).cloned()
    }
}

/// The value of `code` with its `parameters` bound to `arguments` and the captures by name.
pub fn call(
    code: &str,
    parameters: &[String],
    captures: &Map<String, Json>,
    arguments: &[Json],
) -> Result<Json, String> {
    let source = format!("version 1.2\nworkflow w {{\n  String x = {code}\n}}\n");
    let (document, diagnostics) = wdl_ast::Document::parse(&source, None);
    if let Some(diagnostic) = diagnostics.first() {
        return Err(format!(
            "the WDL code does not parse: {}",
            diagnostic.message()
        ));
    }
    let wdl_ast::Ast::V1(ast) = document.ast() else {
        return Err("the WDL code does not parse".into());
    };
    let declaration = (ast.workflows().next()).and_then(|workflow| workflow.statements().next());
    let Some(statement) = declaration else {
        return Err("the WDL code is no expression".into());
    };
    let mut names: HashMap<String, Value> = (captures.iter())
        .map(|(name, value)| (name.clone(), to_wdl(value)))
        .collect();
    names.extend(parameters.iter().cloned().zip(arguments.iter().map(to_wdl)));
    match evaluate(&statement.unwrap_declaration().expr(), &mut Names(names)) {
        Ok(value) => Ok(value.result_json()),
        Err(Error::Failed(why) | Error::Unsupported(why)) => Err(why),
        Err(Error::Unknown) => Err("the WDL code reads files that do not exist yet".into()),
    }
}

/// A wir value as WDL sees it: Files and Directories by location, objects as objects.
fn to_wdl(json: &Json) -> Value {
    match json {
        Json::Null => Value::None,
        Json::Bool(flag) => Value::Boolean(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(int) => Value::Int(int),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => Value::String(text.clone()),
        Json::Array(items) => Value::Array(items.iter().map(to_wdl).collect()),
        Json::Object(object) => {
            let location = || {
                object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            match object.get("class").and_then(Json::as_str) {
                Some("File") => Value::File(location()),
                Some("Directory") => Value::Directory(location()),
                _ => Value::Object(
                    object
                        .iter()
                        .map(|(key, item)| (key.clone(), to_wdl(item)))
                        .collect::<IndexMap<_, _>>(),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wdl_callbacks() {
        let captures = Map::from_iter([("factor".into(), json!(3))]);
        let parameters = vec!["x".to_owned()];
        assert_eq!(
            call("x * factor", &parameters, &captures, &[json!(7)]).unwrap(),
            json!(21)
        );
        let both = vec!["left".to_owned(), "right".to_owned()];
        let joined = call(
            "left + \"-\" + right",
            &both,
            &Map::new(),
            &[json!("a"), json!("b")],
        );
        assert_eq!(joined.unwrap(), json!("a-b"));
        let record = call(
            "sample.reads",
            &["sample".to_owned()],
            &Map::new(),
            &[json!({"reads": [1, 2]})],
        );
        assert_eq!(record.unwrap(), json!([1, 2]));
        assert!(call("x +", &parameters, &Map::new(), &[json!(1)]).is_err());
        assert_eq!(
            call("y", &parameters, &Map::new(), &[json!(1)]).unwrap_err(),
            "unknown name `y`"
        );
    }
}
