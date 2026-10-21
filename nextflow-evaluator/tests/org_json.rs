//! Checks org.json stand-ins against the Groovy oracle in fixtures/oracle/org_json.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The value of the body of a function in a script with the imports of org.json.
fn eval(body: &str) -> String {
    let source = format!(
        "import org.json.JSONObject\nimport org.json.JSONArray\nimport org.json.JSONTokener\n\
         def f() {{\n{body}\n}}"
    );
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Silent;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .unwrap_or_else(|error| error.to_string())
}

#[test]
fn objects_and_arrays() {
    let body = r#"def o = new JSONObject(new JSONTokener('{"a": {"b": [1, 2]}, "c": "x"}'))
def seen = []
o.a.each { inner -> seen << inner.keySet() }
o.put('d', [e: 1])
o.remove('c')
def arr = new JSONArray([1, 'y'])
[o.toString(), o.has('c'), o['a'].get('b').length(), seen, arr.length(), arr.get(1), o.d.getClass().name, arr.contains('y'), o.toMap()]"#;
    assert_eq!(
        eval(body),
        r#"[{"a":{"b":[1,2]},"d":{"e":1}}, false, 2, [[b]], 2, y, org.json.JSONObject, true, [a:[b:[1, 2]], d:[e:1]]]"#
    );
}
