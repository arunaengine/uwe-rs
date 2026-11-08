//! JavaScript callbacks: a callable of a CWL source, called through the CWL expression
//! evaluator of uwe, so that it sees values as CWL expressions do.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::JavaScriptSettings;
use serde_json::{Map, Value as Json, json};
use wir::{ExprLanguage, Port, Type};

/// Calls the callable that `code` gives with `arguments`; each capture that is a name of
/// JavaScript is a variable.
pub fn call(
    (code, library): (&str, &[String]),
    captures: &Map<String, Json>,
    arguments: &[Json],
    settings: &JavaScriptSettings,
) -> Result<Json, String> {
    let mut body = String::from("${\n");
    for name in captures.keys().filter(|name| identifier(name)) {
        body.push_str(&format!(
            "var {name} = inputs.wir_captures[{}];\n",
            json!(name)
        ));
    }
    body.push_str(&format!("var wir_callable = ({code});\n"));
    body.push_str("return wir_callable.apply(null, inputs.wir_arguments);\n}");
    let spec = wir::Expr {
        language: ExprLanguage::Javascript,
        code: body,
        library: library.to_vec(),
        scope: None,
        captures: Default::default(),
        form: None,
    };
    let bound = Map::from_iter([
        ("wir_arguments".to_owned(), Json::Array(arguments.to_vec())),
        ("wir_captures".to_owned(), Json::Object(captures.clone())),
    ]);
    let out = Port {
        name: "out".into(),
        r#type: Type::Dynamic,
        flow: Default::default(),
        default: None,
    };
    crate::expr::evaluate(&spec, &bound, &out, &[], (settings, false))
}

/// Whether a name is a JavaScript identifier of ASCII letters, digits, `_` and `$`.
fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let first = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    first && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    #[ignore = "needs the CWL JavaScript helper image, which `just test` builds"]
    fn javascript_callbacks() {
        let settings = JavaScriptSettings::default();
        let captures = Map::from_iter([("factor".into(), json!(3))]);
        let scaled = call(
            ("function (x) { return x * factor; }", &[]),
            &captures,
            &[json!(7)],
            &settings,
        );
        assert_eq!(scaled.unwrap(), json!(21));
        let library = vec!["function label(a, b) { return a + '-' + b; }".to_owned()];
        let labelled = call(
            ("label", &library),
            &Map::new(),
            &[json!("a"), json!(2)],
            &settings,
        );
        assert_eq!(labelled.unwrap(), json!("a-2"));
        let file = json!({"class": "File", "location": "/data/a.txt"});
        let basename = call(
            ("function (f) { return f.nameroot; }", &[]),
            &Map::new(),
            &[file],
            &settings,
        );
        assert_eq!(basename.unwrap(), json!("a"), "a File looks as in CWL");
        assert!(call(("function (", &[]), &Map::new(), &[], &settings).is_err());
    }
}
