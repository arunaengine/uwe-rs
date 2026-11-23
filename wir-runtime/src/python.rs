//! Python callbacks: callables and expressions of Snakemake sources, run by the Python
//! evaluator of uwe, and the values that cross between wir and Python.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::IndexSet;
use python_evaluator::{Args, Dict, Host, Interpreter, Key, LocalHost, Stat, Value};
use serde_json::{Map, Value as Json, json};
use workflow_graph::Roots;

/// Python code with the library that runs before it.
pub struct Code<'c> {
    pub code: &'c str,
    pub library: &'c [String],
}

impl Code<'_> {
    /// The value of the code as an expression, with `values` as its globals.
    pub fn value(&self, values: &Map<String, Json>, host: Rc<dyn Host>) -> Result<Json, String> {
        let (mut interpreter, globals) = self.prepared(values, host)?;
        let result = interpreter
            .eval_text(self.code, &globals)
            .map_err(|error| error.to_string())?;
        to_json(&mut interpreter, &result)
    }

    /// Calls the callable that the code gives with `arguments`.
    pub fn call(
        &self,
        values: &Map<String, Json>,
        arguments: &[Json],
        host: Rc<dyn Host>,
    ) -> Result<Json, String> {
        let (mut interpreter, globals) = self.prepared(values, host)?;
        let callable = interpreter
            .eval_text(self.code, &globals)
            .map_err(|error| error.to_string())?;
        let arguments = Args::new(arguments.iter().map(to_value).collect());
        let result = interpreter
            .call(&callable, arguments)
            .map_err(|error| error.to_string())?;
        to_json(&mut interpreter, &result)
    }

    /// An interpreter whose globals hold the values and the definitions of the library.
    fn prepared(
        &self,
        values: &Map<String, Json>,
        host: Rc<dyn Host>,
    ) -> Result<(Interpreter, Rc<python_evaluator::Scope>), String> {
        let mut interpreter = Interpreter::new(host);
        let globals = interpreter.new_globals("wir", None);
        for (name, value) in values {
            globals.set(name, to_value(value));
        }
        for (index, text) in self.library.iter().enumerate() {
            let path = format!("<library {index}>");
            interpreter
                .exec_text(&path, text, &globals)
                .map_err(|error| error.to_string())?;
        }
        Ok((interpreter, globals))
    }
}

/// The host of Python code that starts in `base`. In an isolated run, which has `roots`, the
/// code reads and lists only the folders of the run and sees its environment, not uwe's.
pub fn host(
    base: &Path,
    roots: Option<&Roots>,
    environment: Option<&[(String, String)]>,
) -> Rc<dyn Host> {
    let local = LocalHost::new(base);
    match roots {
        None => Rc::new(local),
        Some(roots) => Rc::new(Confined {
            local,
            roots: roots.clone(),
            environment: environment.unwrap_or_default().to_vec(),
        }),
    }
}

/// A host of an isolated run: the local host within the folders of the run. Writes stay in
/// memory as with the local host.
struct Confined {
    local: LocalHost,
    roots: Roots,
    environment: Vec<(String, String)>,
}

impl Host for Confined {
    fn cwd(&self) -> PathBuf {
        self.local.cwd()
    }

    fn set_cwd(&self, path: &Path) {
        self.local.set_cwd(path);
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.roots.check_read(path)?;
        self.local.read(path)
    }

    fn stat(&self, path: &Path) -> Option<Stat> {
        self.roots.check_read(path).ok()?;
        self.local.stat(path)
    }

    fn is_symlink(&self, path: &Path) -> bool {
        self.roots.check_read(path).is_ok() && self.local.is_symlink(path)
    }

    fn canonicalize(&self, path: &Path) -> PathBuf {
        match self.roots.check_read(path) {
            Ok(()) => self.local.canonicalize(path),
            Err(_) => path.to_owned(),
        }
    }

    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        self.roots.check_list(path)?;
        self.local.list(path)
    }

    fn write(&self, path: &Path, data: &[u8], append: bool) -> io::Result<()> {
        self.roots.check(path)?;
        self.local.write(path, data, append)
    }

    fn make_dir(&self, path: &Path) -> io::Result<()> {
        self.roots.check(path)?;
        self.local.make_dir(path)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.roots.check(path)?;
        self.local.remove(path)
    }

    fn print(&self, text: &str) {
        self.local.print(text);
    }

    fn environment(&self) -> Vec<(String, String)> {
        self.environment.clone()
    }
}

/// A wir value as Python sees it: Files and Directories are their paths, a set is a set.
pub fn to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::None,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(int) => Value::Int(int),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => Value::str(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(to_value).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File" | "Directory") => {
                let location = object
                    .get("location")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                Value::str(location.strip_prefix("file://").unwrap_or(location))
            }
            Some("Set") => {
                let items = object
                    .get("items")
                    .and_then(Json::as_array)
                    .cloned()
                    .unwrap_or_default();
                Value::set(
                    items
                        .iter()
                        .map(|item| Key(to_value(item)))
                        .collect::<IndexSet<Key>>(),
                )
            }
            _ => {
                let mut dict = Dict::default();
                for (key, item) in object {
                    dict.entries
                        .insert(Key(Value::str(key.as_str())), to_value(item));
                }
                Value::dict(dict)
            }
        },
    }
}

/// A Python value as a wir value, or why it cannot cross a port.
pub fn to_json(interpreter: &mut Interpreter, value: &Value) -> Result<Json, String> {
    let failed = |error: python_evaluator::Error| error.to_string();
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Int(int) => json!(int),
        Value::BigInt(big) => return Err(format!("the integer {big} is out of the int range")),
        Value::Float(float) => match serde_json::Number::from_f64(*float) {
            Some(number) => Json::Number(number),
            None => return Err(format!("the number {float} has no wir value")),
        },
        Value::Str(text) => json!(text.as_ref()),
        Value::List(_) | Value::Tuple(_) => {
            let items = interpreter.iterate(value).map_err(failed)?;
            Json::Array(
                items
                    .iter()
                    .map(|item| to_json(interpreter, item))
                    .collect::<Result<_, _>>()?,
            )
        }
        Value::Set(_) | Value::FrozenSet(_) => {
            let items = interpreter.iterate(value).map_err(failed)?;
            let items: Vec<Json> = items
                .iter()
                .map(|item| to_json(interpreter, item))
                .collect::<Result<_, _>>()?;
            json!({"class": "Set", "items": items})
        }
        Value::Dict(_) => {
            let mut object = Map::new();
            for (key, item) in interpreter.mapping_items(value).map_err(failed)? {
                let Value::Str(key) = key.plain() else {
                    return Err("a dict with keys other than strings cannot cross a port".into());
                };
                object.insert(key.to_string(), to_json(interpreter, &item)?);
            }
            Json::Object(object)
        }
        other => {
            let shown = interpreter.repr(other).map_err(failed)?;
            return Err(format!("the Python value {shown} cannot cross a port"));
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn python_callbacks() {
        let base = || host(Path::new("/data"), None, None);
        let scaled = Code {
            code: "lambda x: x * factor",
            library: &[],
        };
        let values = Map::from_iter([("factor".into(), json!(3))]);
        assert_eq!(
            scaled.call(&values, &[json!(7)], base()).unwrap(),
            json!(21)
        );
        let library = vec!["def label(sample, n):\n    return f'{sample}-{n}'\n".to_owned()];
        let labelled = Code {
            code: "label",
            library: &library,
        };
        assert_eq!(
            labelled
                .call(&Map::new(), &[json!("a"), json!(2)], base())
                .unwrap(),
            json!("a-2")
        );
        let expression = Code {
            code: "sorted(inputs['names'])[0].upper()",
            library: &[],
        };
        let values = Map::from_iter([("inputs".into(), json!({"names": ["b", "a"]}))]);
        assert_eq!(expression.value(&values, base()).unwrap(), json!("A"));
        let file = json!({"class": "File", "location": "/data/a.txt"});
        let path = Code {
            code: "lambda f: f.endswith('.txt')",
            library: &[],
        };
        assert_eq!(
            path.call(&Map::new(), &[file], base()).unwrap(),
            json!(true),
            "a File is its path"
        );
        let set = Code {
            code: "lambda items: set(items)",
            library: &[],
        };
        assert_eq!(
            set.call(&Map::new(), &[json!([1, 1, 2])], base()).unwrap(),
            json!({"class": "Set", "items": [1, 2]})
        );
        let function = Code {
            code: "lambda: (lambda y: y)",
            library: &[],
        };
        let error = function.call(&Map::new(), &[], base()).unwrap_err();
        assert!(error.contains("cannot cross a port"), "{error}");
        assert!(
            Code {
                code: "lambda x:",
                library: &[]
            }
            .call(&Map::new(), &[], base())
            .is_err()
        );
    }

    #[test]
    fn confines_isolated_code() {
        let folder = tempfile::tempdir().expect("a folder");
        let run = folder.path().join("run");
        std::fs::create_dir_all(&run).expect("a run folder");
        std::fs::write(run.join("in.txt"), "inside").expect("a file of the run");
        std::fs::write(folder.path().join("secret.txt"), "outside").expect("another file");
        let roots = Roots::new([run.clone()]);
        let environment = [("SAMPLE".to_owned(), "a".to_owned())];
        let confined = || host(&run, Some(&roots), Some(&environment));
        let library = vec!["import os".to_owned()];
        let evaluate = |code: String| {
            let code = Code {
                code: &code,
                library: &library,
            };
            code.value(&Map::new(), confined())
        };
        let read = |path: &Path| evaluate(format!("open('{}').read()", path.display()));
        assert_eq!(read(&run.join("in.txt")), Ok(json!("inside")));
        let refused = read(&folder.path().join("secret.txt")).expect_err("outside the run");
        assert!(refused.contains("PermissionError"), "{refused}");
        let listed = evaluate(format!("os.listdir('{}')", folder.path().display()));
        assert!(listed.is_err(), "{listed:?}");
        let names = evaluate("sorted(os.environ)".to_owned());
        assert_eq!(
            names,
            Ok(json!(["SAMPLE"])),
            "only the environment of the run"
        );
    }
}
