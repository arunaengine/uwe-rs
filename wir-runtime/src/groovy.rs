//! Groovy callbacks: closures and expressions of Nextflow sources, run by the Nextflow
//! evaluator of uwe, and the values that cross between wir and Groovy.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::cmp::Ordering;
use std::path::Path;
use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::Module;
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Object, PathValue, State, Value};
use serde_json::{Map, Value as Json, json};

use crate::engine::Context;
use crate::nextflow_scope::{Launching, Scoped};

/// The name of the function that gives the callback.
pub(crate) const ENTRY: &str = "wir_callback";

/// Code of a source that a callback runs in, lowered once.
pub struct Compiled {
    module: Rc<Module>,
    /// The names of the parameters of the entry function, in order.
    names: Vec<String>,
    /// The callback of a Nextflow scope, which runs in an interpreter of the run instead.
    nextflow: Option<Scoped>,
}

/// Where callback code runs: after the text of its scope, if it has one, or in the run of the
/// Nextflow script of its scope.
pub enum Scope<'a> {
    Text(Option<&'a str>),
    /// The run of the script, and where the closure of the code is in it.
    Nextflow(Box<Launching>, std::ops::Range<u64>),
}

/// The scope of a callback or expression of an op.
pub fn scope<'a>(context: &Context<'a>, scope: Option<&wir::Scope>) -> Scope<'a> {
    let Some(scope) = scope else {
        return Scope::Text(None);
    };
    match crate::nextflow_scope::launching(context, &scope.source) {
        Some(launching) => Scope::Nextflow(Box::new(launching), scope.start..scope.end),
        None => Scope::Text(context.scopes.get(&scope.source).map(String::as_str)),
    }
}

/// Lowers `code` as the body of a function of `names`, after the `library` definitions and
/// the text of the source of its scope; a Nextflow scope loads its script with its includes.
pub fn compile(
    code: &str,
    library: &[String],
    scope: Scope<'_>,
    names: Vec<String>,
) -> Result<Compiled, String> {
    let scope = match scope {
        Scope::Nextflow(launching, closure) => {
            let scoped = Scoped::load(*launching, (code, closure), library, names.clone())?;
            return Ok(Compiled {
                module: Rc::new(Module::default()),
                names,
                nextflow: Some(scoped),
            });
        }
        Scope::Text(text) => text,
    };
    let mut source = String::new();
    if let Some(text) = scope {
        source.push_str(text);
        source.push('\n');
    }
    for item in library {
        source.push_str(item);
        source.push('\n');
    }
    source.push_str(&format!(
        "def {ENTRY}({}) {{\n    return ({code})\n}}\n",
        names.join(", ")
    ));
    let parsed = nextflow_parser::parse_script(&source);
    if let Some(error) = parsed.errors.first() {
        return Err(format!("the Groovy code does not parse: {}", error.message));
    }
    let module = lower_module(&parsed.tree, &source).map_err(|errors| {
        let first = errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_default();
        format!("the Groovy code does not lower: {first}")
    })?;
    Ok(Compiled {
        module: Rc::new(module),
        names,
        nextflow: None,
    })
}

/// Prints nothing: a callback is pure, and printing is the work of `view`.
struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _text: &str) {}
}

/// Prints through the print sink of the run, for an expression with the `log` effect.
struct Printing<'a>(&'a crate::driver::Printer);

impl Host for Printing<'_> {
    fn print(&mut self, text: &str) {
        self.0.print(text);
    }
}

impl Compiled {
    /// The value of the code for the values of its names, which prints nothing.
    pub fn value(&self, values: &Map<String, Json>, base: &Path) -> Result<Json, String> {
        self.value_in(&mut Quiet, values, base)
    }

    /// The value of the code, which prints through `printer`, like a check at the start of a
    /// run; code of a Nextflow scope prints through the host of the Nextflow run.
    pub fn printed(
        &self,
        values: &Map<String, Json>,
        base: &Path,
        printer: &crate::driver::Printer,
    ) -> Result<Json, String> {
        self.value_in(&mut Printing(printer), values, base)
    }

    fn value_in(
        &self,
        host: &mut dyn Host,
        values: &Map<String, Json>,
        base: &Path,
    ) -> Result<Json, String> {
        if let Some(scoped) = &self.nextflow {
            return scoped.run(values, None, base, None);
        }
        let mut interpreter = Interpreter::new(host);
        let result = self.run(&mut interpreter, values, base)?;
        to_json(&result)
    }

    /// Calls the closure that the code gives with `arguments`.
    pub fn call(
        &self,
        values: &Map<String, Json>,
        arguments: &[Json],
        base: &Path,
    ) -> Result<Json, String> {
        self.call_in(&mut Quiet, values, arguments, base)
    }

    /// Calls the closure like [`Compiled::call`], printing through `printer`, like a hook.
    pub fn call_printed(
        &self,
        (values, arguments): (&Map<String, Json>, &[Json]),
        base: &Path,
        printer: &crate::driver::Printer,
    ) -> Result<Json, String> {
        if let Some(scoped) = &self.nextflow {
            return scoped.run(values, Some(arguments), base, Some(printer));
        }
        self.call_in(&mut Printing(printer), values, arguments, base)
    }

    fn call_in(
        &self,
        host: &mut dyn Host,
        values: &Map<String, Json>,
        arguments: &[Json],
        base: &Path,
    ) -> Result<Json, String> {
        if let Some(scoped) = &self.nextflow {
            return scoped.run(values, Some(arguments), base, None);
        }
        let mut interpreter = Interpreter::new(host);
        let Value::Closure(closure) = self.run(&mut interpreter, values, base)? else {
            return Err("the Groovy code gives no closure".into());
        };
        let arguments = arguments.iter().map(|item| to_value(item, base)).collect();
        let result =
            (interpreter.call_closure(&closure, arguments)).map_err(|error| error.to_string())?;
        to_json(&result)
    }

    fn run(
        &self,
        interpreter: &mut Interpreter<'_>,
        values: &Map<String, Json>,
        base: &Path,
    ) -> Result<Value, String> {
        let arguments = (self.names.iter())
            .map(|name| to_value(values.get(name).unwrap_or(&Json::Null), base))
            .collect();
        (interpreter.call_function(&self.module, ENTRY, arguments))
            .map_err(|error| error.to_string())
    }
}

/// The order of two values by Groovy's rules, as `sort()` without a closure gives it.
pub fn compare(left: &Json, right: &Json, base: &Path) -> Ordering {
    to_value(left, base).number_aware_compare(&to_value(right, base))
}

/// The CacheHelper hash that Nextflow sorts by, of the file contents when `deep`, as an order
/// key: big-endian bytes whose order is that of the signed hash.
pub fn hash_key(value: &Json, deep: bool, base: &Path) -> Result<Vec<u8>, String> {
    let key = nextflow_runtime::hash_key(&to_value(value, base), &mut Quiet, deep);
    let key = key.map_err(|error| error.to_string())?;
    Ok(((key as u64) ^ (1 << 63)).to_be_bytes().to_vec())
}

/// The text of a value as Groovy's `toString` gives it, which `view` prints.
pub fn text(value: &Json, base: &Path) -> String {
    to_value(value, base).to_string()
}

/// The sum of two values by Groovy's `+`, as Nextflow's `sum` adds them.
pub fn add(left: &Json, right: &Json, base: &Path) -> Result<Json, String> {
    let plus = compile("{ a, b -> a + b }", &[], Scope::Text(None), Vec::new())?;
    plus.call(&Map::new(), &[left.clone(), right.clone()], base)
}

/// A wir value as Groovy sees it: Files and Directories are paths, and the bridge forms are
/// their Nextflow values.
pub fn to_value(json: &Json, base: &Path) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Boolean(*flag),
        Json::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(int), _) => match i32::try_from(int) {
                Ok(small) => Value::Number(Number::Integer(small)),
                Err(_) => Value::Number(Number::Long(int)),
            },
            (None, Some(float)) => Value::Number(Number::Double(float)),
            _ => Value::Null,
        },
        Json::String(text) => Value::string(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(|item| to_value(item, base)).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            // A variable of a script, which only a Nextflow scope shares between callbacks.
            Some("Script") => to_value(object.get("value").unwrap_or(&Json::Null), base),
            Some("File" | "Directory") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let location = location.strip_prefix("file://").unwrap_or(location);
                Value::Path(Rc::new(PathValue::parse(location, &base.to_string_lossy())))
            }
            Some("GroupKey") => {
                let target = to_value(object.get("target").unwrap_or(&Json::Null), base);
                let size = object
                    .get("size")
                    .and_then(Json::as_i64)
                    .unwrap_or_default();
                let state = State::GroupKey(target, i32::try_from(size).unwrap_or(i32::MAX));
                let class = "nextflow.extension.GroupKey";
                Value::Object(Rc::new(RefCell::new(Object { class, state })))
            }
            Some("MapEntry") => {
                let key = to_value(object.get("key").unwrap_or(&Json::Null), base);
                let value = to_value(object.get("value").unwrap_or(&Json::Null), base);
                Value::MapEntry(Rc::new((key, value)))
            }
            Some("Set") => {
                let items = object
                    .get("items")
                    .and_then(Json::as_array)
                    .cloned()
                    .unwrap_or_default();
                Value::set(items.iter().map(|item| to_value(item, base)))
            }
            _ => Value::map(
                object
                    .iter()
                    .map(|(key, item)| (Value::string(key.as_str()), to_value(item, base))),
            ),
        },
    }
}

/// A Groovy value as a wir value, or why it cannot cross a port.
pub fn to_json(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Null => Json::Null,
        Value::Boolean(flag) => Json::Bool(*flag),
        Value::Number(Number::Integer(int)) => json!(int),
        Value::Number(Number::Long(int)) => json!(int),
        Value::Number(Number::BigInteger(big)) => match big.to_string().parse::<i64>() {
            Ok(int) => json!(int),
            Err(_) => return Err(format!("the integer {value} is out of the int range")),
        },
        Value::Number(number) => match serde_json::Number::from_f64(number.to_f64()) {
            Some(number) => Json::Number(number),
            None => return Err(format!("the number {value} has no wir value")),
        },
        Value::String(text) | Value::GString(text) => json!(text.as_ref()),
        Value::Character(_) | Value::MemoryUnit(_) | Value::Duration(_) => json!(value.to_string()),
        Value::List(list) => Json::Array(
            list.borrow()
                .items
                .iter()
                .map(to_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => {
            let entries = map
                .borrow()
                .entries
                .iter()
                .map(|(key, item)| Ok((key.0.to_string(), to_json(item)?)))
                .collect::<Result<Map<_, _>, String>>()?;
            Json::Object(entries)
        }
        Value::Set(set) => {
            let items = set
                .borrow()
                .items
                .iter()
                .map(|item| to_json(&item.0))
                .collect::<Result<Vec<_>, _>>()?;
            json!({"class": "Set", "items": items})
        }
        Value::MapEntry(entry) => {
            json!({"class": "MapEntry", "key": to_json(&entry.0)?, "value": to_json(&entry.1)?})
        }
        Value::Path(path) => {
            let location = match path.prefix {
                None => path.path.to_string(),
                Some(_) => path.uri(),
            };
            let class = match Path::new(&location).is_dir() {
                true => "Directory",
                false => "File",
            };
            json!({"class": class, "location": location, "basename": path.name()})
        }
        Value::Object(object) => match &object.borrow().state {
            State::GroupKey(target, size) => {
                json!({"class": "GroupKey", "target": to_json(target)?, "size": size})
            }
            _ => return Err(format!("a {} cannot cross a port", value.java_class_name())),
        },
        other => return Err(format!("a {} cannot cross a port", other.java_class_name())),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn groovy_callbacks() {
        let base = Path::new("/data");
        let twice = compile(
            "{ x -> x * factor }",
            &[],
            Scope::Text(None),
            vec!["factor".into()],
        )
        .unwrap();
        let values = Map::from_iter([("factor".into(), json!(3))]);
        assert_eq!(twice.call(&values, &[json!(7)], base).unwrap(), json!(21));
        let pairs = compile(
            "{ id, reads -> [id, reads.size()] }",
            &[],
            Scope::Text(None),
            Vec::new(),
        )
        .unwrap();
        let item = json!(["a", [1, 2]]);
        assert_eq!(
            pairs.call(&Map::new(), &[item], base).unwrap(),
            json!(["a", 2]),
            "a tuple fills the parameters"
        );
        let library = vec!["def shout(text) { text.toUpperCase() }".to_owned()];
        let loud = compile(
            "shout(inputs.word)",
            &library,
            Scope::Text(None),
            vec!["inputs".into()],
        )
        .unwrap();
        let values = Map::from_iter([("inputs".into(), json!({"word": "hi"}))]);
        assert_eq!(loud.value(&values, base).unwrap(), json!("HI"));
        assert!(compile("{ x -> ", &[], Scope::Text(None), Vec::new()).is_err());
    }

    #[test]
    fn bridge_forms() {
        let base = Path::new("/data");
        let forms = [
            json!({"class": "GroupKey", "target": "s1", "size": 2}),
            json!({"class": "MapEntry", "key": "a", "value": [1]}),
            json!({"class": "Set", "items": [1, 2]}),
            json!({"class": "File", "location": "/data/a.txt", "basename": "a.txt"}),
            json!({"n": 1.5, "list": [true, null, "x"]}),
        ];
        for form in forms {
            assert_eq!(to_json(&to_value(&form, base)).unwrap(), form, "{form}");
        }
        let target = compile(
            "{ key -> key.getGroupTarget() }",
            &[],
            Scope::Text(None),
            Vec::new(),
        )
        .unwrap();
        let key = json!({"class": "GroupKey", "target": "s1", "size": 2});
        assert_eq!(target.call(&Map::new(), &[key], base).unwrap(), json!("s1"));
        let closure = compile("{ x -> { -> x } }", &[], Scope::Text(None), Vec::new()).unwrap();
        let error = closure.call(&Map::new(), &[json!(1)], base).unwrap_err();
        assert!(error.contains("cannot cross a port"), "{error}");
    }

    #[test]
    fn groovy_rules() {
        let base = Path::new("/data");
        let file = |name: &str| json!({"class": "File", "location": format!("/w/{name}")});
        assert_eq!(
            compare(&file("b.txt"), &file("a.txt"), base),
            Ordering::Greater
        );
        assert_eq!(compare(&json!(2), &json!(10.5), base), Ordering::Less);
        assert_eq!(
            text(&json!(["a", 1, {"k": null}]), base),
            "[a, 1, [k:null]]"
        );
        assert_eq!(add(&json!("a"), &json!("b"), base).unwrap(), json!("ab"));
        let keys = [
            hash_key(&json!("x"), false, base),
            hash_key(&json!("x"), false, base),
        ];
        assert_eq!(keys[0], keys[1], "the hash of a value is stable");
    }
}
