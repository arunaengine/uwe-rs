//! Runs code that imports packages without a stand-in against a fake Python process: the
//! requests it records must follow the protocol, and its replies must become values, handles
//! and exceptions (decision 19).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use python_evaluator::packages::{PythonProcess, PythonStart, PythonStarter};
use python_evaluator::{Interpreter, LocalHost};
use serde_json::{Value as Json, json};

/// What the fake saw: the start, the requests and whether it was stopped.
#[derive(Default)]
struct Seen {
    starts: Vec<PythonStart>,
    requests: Vec<Json>,
    stopped: bool,
}

struct FakeProcess {
    replies: VecDeque<Json>,
    seen: Rc<RefCell<Seen>>,
}

impl PythonProcess for FakeProcess {
    fn exchange(&mut self, line: &str) -> Result<String, String> {
        let request: Json = serde_json::from_str(line).expect("a JSON request");
        let stop = request["op"] == "stop";
        self.seen.borrow_mut().requests.push(request);
        if stop {
            return Ok(r#"{"value": null}"#.into());
        }
        let reply = self.replies.pop_front().ok_or("no reply left")?;
        Ok(reply.to_string())
    }

    fn stop(&mut self) {
        self.seen.borrow_mut().stopped = true;
    }
}

/// Starts a fake process that gives the prepared replies, or fails with a reason.
struct FakeStarter {
    replies: RefCell<Option<Vec<Json>>>,
    failure: Option<String>,
    seen: Rc<RefCell<Seen>>,
}

impl PythonStarter for FakeStarter {
    fn start(&self, start: &PythonStart) -> Result<Box<dyn PythonProcess>, String> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        self.seen.borrow_mut().starts.push(start.clone());
        let replies = self.replies.borrow_mut().take().unwrap_or_default();
        Ok(Box::new(FakeProcess {
            replies: replies.into(),
            seen: self.seen.clone(),
        }))
    }
}

fn handle(number: u64, kind: &str, callable: bool) -> Json {
    json!({"value": {"handle": number, "type": kind, "callable": callable}})
}

fn value(value: Json) -> Json {
    json!({ "value": value })
}

fn error(kind: &str, module: &str, bases: &[&str], message: &str) -> Json {
    json!({"error": {"type": kind, "module": module, "bases": bases, "message": message, "args": [message]}})
}

struct Run {
    interpreter: Interpreter,
    host: Rc<LocalHost>,
    seen: Rc<RefCell<Seen>>,
}

fn interpreter(replies: Vec<Json>, failure: Option<&str>) -> Run {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let interpreter = Interpreter::new(host.clone());
    let seen = Rc::new(RefCell::new(Seen::default()));
    interpreter.packages.set_starter(Rc::new(FakeStarter {
        replies: RefCell::new(Some(replies)),
        failure: failure.map(str::to_owned),
        seen: seen.clone(),
    }));
    Run {
        interpreter,
        host,
        seen,
    }
}

/// Runs code and gives what it printed, or its error.
fn run(run: &mut Run, code: &str) -> Result<String, python_evaluator::Error> {
    run.interpreter.run_text("<test>", code)?;
    Ok(run.host.output.borrow().clone())
}

/// The requests without the ones that only release handles.
fn requests(run: &Run) -> Vec<Json> {
    run.seen.borrow().requests.clone()
}

#[test]
fn calls_give_plain_values_and_handles() {
    let mut test = interpreter(
        vec![
            handle(1, "module", false),
            handle(2, "function", true),
            value(json!({"float": 3.5})),
            handle(3, "type", true),
            handle(4, "Counter", false),
            value(json!(3)),
            value(json!(3)),
            value(json!("Counter(a)")),
            handle(5, "function", true),
            value(json!({"tuple": ["joined", {"list": [1, 2]}]})),
        ],
        None,
    );
    let code = "import toy\nprint(toy.add(1, 2.5))\ncounter = toy.Counter('a', size=3)\nprint(counter.size, len(counter), str(counter))\nprint(toy.join(counter, [1, (2, None)], {'k': True}))\n";
    assert_eq!(
        run(&mut test, code).expect("runs"),
        "3.5\n3 3 Counter(a)\n('joined', [1, 2])\n"
    );
    let seen = test.seen.borrow();
    assert_eq!(seen.starts.len(), 1);
    assert_eq!(seen.starts[0].packages, ["toy"]);
    assert!(seen.starts[0].script.contains("def answer(request)"));
    drop(seen);
    assert_eq!(
        requests(&test),
        vec![
            json!({"op": "import", "module": "toy", "packages": ["toy"]}),
            json!({"op": "getattr", "object": {"handle": 1}, "name": "add"}),
            json!({"op": "call", "object": {"handle": 2}, "args": [1, {"float": 2.5}], "kwargs": []}),
            json!({"op": "getattr", "object": {"handle": 1}, "name": "Counter", "release": [2]}),
            json!({"op": "call", "object": {"handle": 3}, "args": ["a"], "kwargs": [["size", 3]]}),
            json!({"op": "getattr", "object": {"handle": 4}, "name": "size", "release": [3]}),
            json!({"op": "len", "object": {"handle": 4}}),
            json!({"op": "str", "object": {"handle": 4}}),
            json!({"op": "getattr", "object": {"handle": 1}, "name": "join"}),
            json!({"op": "call", "object": {"handle": 5}, "args": [{"handle": 4}, {"list": [1, {"tuple": [2, null]}]}, {"dict": [["k", true]]}], "kwargs": []}),
        ]
    );
}

#[test]
fn values_of_every_kind_come_back() {
    let mut test = interpreter(
        vec![
            handle(1, "module", false),
            value(json!({"dict": [
                ["a", {"int": "123456789012345678901234"}],
                ["b", {"float": "nan"}],
                ["c", {"bytes": "6869"}],
                ["d", {"set": [1]}],
                ["e", {"path": "/x/y"}],
            ]})),
        ],
        None,
    );
    let output = run(&mut test, "import toy\nprint(toy.values)\n").expect("runs");
    assert_eq!(
        output,
        "{'a': 123456789012345678901234, 'b': nan, 'c': b'hi', 'd': {1}, 'e': PosixPath('/x/y')}\n"
    );
}

#[test]
fn exceptions_of_packages_are_python_exceptions() {
    let mut test = interpreter(
        vec![
            handle(1, "module", false),
            handle(2, "function", true),
            json!({"error": {"type": "KeyError", "module": "builtins", "bases": ["LookupError", "Exception", "BaseException"], "message": "'x'", "args": ["x"]}}),
            handle(3, "function", true),
            error(
                "FastaIndexingError",
                "pyfaidx",
                &["ValueError", "Exception", "BaseException"],
                "bad index",
            ),
            error(
                "AttributeError",
                "builtins",
                &["Exception", "BaseException"],
                "no",
            ),
            error(
                "ModuleNotFoundError",
                "builtins",
                &["ImportError", "Exception", "BaseException"],
                "No module named 'nothing'",
            ),
        ],
        None,
    );
    let code = "import toy\ntry:\n    toy.fail()\nexcept KeyError as error:\n    print('key', error)\ntry:\n    toy.index()\nexcept ValueError as error:\n    print(type(error).__name__, error)\nprint(hasattr(toy, 'nope'))\ntry:\n    import nothing\nexcept ImportError:\n    print('fallback')\n";
    assert_eq!(
        run(&mut test, code).expect("runs"),
        "key 'x'\nFastaIndexingError bad index\nFalse\nfallback\n"
    );
}

#[test]
fn submodules_are_imported_in_the_process() {
    let mut test = interpreter(
        vec![
            handle(1, "module", false),
            error(
                "AttributeError",
                "builtins",
                &["Exception", "BaseException"],
                "no sub",
            ),
            handle(2, "module", false),
            handle(3, "module", false),
        ],
        None,
    );
    run(
        &mut test,
        "from toy import sub\nimport toy.other as other\n",
    )
    .expect("runs");
    let modules: Vec<Json> = requests(&test)
        .iter()
        .filter(|request| request["op"] == "import")
        .map(|request| request["module"].clone())
        .collect();
    assert_eq!(
        modules,
        [json!("toy"), json!("toy.sub"), json!("toy.other")]
    );
}

#[test]
fn stopping_ends_the_process_and_its_handles() {
    let mut test = interpreter(vec![handle(1, "module", false)], None);
    let Ok(globals) = test.interpreter.run_text("<test>", "import toy\n") else {
        panic!("imports");
    };
    test.interpreter.packages.stop();
    assert!(test.seen.borrow().stopped);
    assert_eq!(requests(&test).last(), Some(&json!({"op": "stop"})));
    let error = test
        .interpreter
        .exec_text("<test>", "print(toy.name)\n", &globals)
        .expect_err("a stopped process");
    assert!(error.exception().is_none(), "{error}");
    assert!(error.to_string().contains("stopped"), "{error}");
}

#[test]
fn without_a_container_runtime_the_run_stops() {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host);
    let Err(error) = interpreter.run_text(
        "<test>",
        "try:\n    import pyfaidx\nexcept ImportError:\n    pass\n",
    ) else {
        panic!("runs without a container runtime");
    };
    assert!(error.exception().is_none() && !error.is_unsupported());
    assert!(
        error.to_string().contains("the Python package pyfaidx")
            && error.to_string().contains("decision 19"),
        "{error}"
    );
    let mut test = interpreter_failing("docker: command not found");
    let error = run(&mut test, "import PIL.Image\n").expect_err("no engine");
    assert!(
        error.to_string().contains("the Python package PIL")
            && error.to_string().contains("docker: command not found"),
        "{error}"
    );
}

fn interpreter_failing(reason: &str) -> Run {
    interpreter(Vec::new(), Some(reason))
}

#[test]
fn a_dotted_import_installs_the_package_of_its_full_name() {
    let mut test = interpreter(
        vec![handle(1, "module", false), handle(2, "module", false)],
        None,
    );
    run(&mut test, "import google.auth\n").expect("runs");
    assert_eq!(test.seen.borrow().starts[0].packages, ["google-auth"]);
    assert_eq!(
        requests(&test)[0],
        json!({"op": "import", "module": "google", "packages": ["google-auth"]})
    );
}

#[test]
fn folders_of_the_workflow_are_not_packages() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(folder.path().join("src")).expect("a folder");
    let host = Rc::new(LocalHost::new(folder.path()));
    let interpreter = Interpreter::new(host.clone());
    let seen = Rc::new(RefCell::new(Seen::default()));
    interpreter.packages.set_starter(Rc::new(FakeStarter {
        replies: RefCell::new(Some(Vec::new())),
        failure: None,
        seen: seen.clone(),
    }));
    let mut test = Run {
        interpreter,
        host,
        seen,
    };
    let output = run(
        &mut test,
        "try:\n    from src.helpers import x\nexcept ModuleNotFoundError as error:\n    print(error)\n",
    )
    .expect("runs");
    assert_eq!(output, "No module named 'src'\n");
    assert!(test.seen.borrow().starts.is_empty());
}

#[test]
fn except_clauses_catch_exceptions_by_classes_of_the_package() {
    let raised = |kind: &str| {
        json!({"error": {"type": kind, "module": "toy.errors", "bases": ["Exception", "BaseException"],
            "classes": [format!("toy.errors.{kind}"), "toy.errors.ToyError", "builtins.Exception", "builtins.BaseException"],
            "message": "no", "args": ["no"]}})
    };
    let class = |number: u64, name: &str| json!({"value": {"handle": number, "type": "type", "callable": true, "name": name, "module": "toy.errors"}});
    let mut test = interpreter(
        vec![
            handle(1, "module", false),
            handle(2, "function", true),
            raised("Missing"),
            class(3, "ToyError"),
            handle(4, "function", true),
            raised("Other"),
            class(5, "Missing"),
        ],
        None,
    );
    let code = "import toy\ntry:\n    toy.fail()\nexcept toy.ToyError as error:\n    print('caught', type(error).__name__)\ntry:\n    toy.fail()\nexcept toy.Missing:\n    print('wrong')\nexcept Exception as error:\n    print('other', error)\n";
    assert_eq!(
        run(&mut test, code).expect("runs"),
        "caught Missing\nother no\n"
    );
}
