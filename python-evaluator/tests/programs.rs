//! Runs the Python programs of `tests/fixtures/python` and compares their output with what
//! CPython 3.11 printed for them, recorded in the `.out` file next to each program.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};

fn run(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("reads the program");
    let host = Rc::new(LocalHost::new(path.parent().expect("a folder")));
    let mut interpreter = Interpreter::new(host.clone());
    let result = interpreter.run_text(&path.to_string_lossy(), &text);
    let mut output = host.output.borrow().clone();
    if let Err(error) = result {
        output.push_str(&format!("{error}\n"));
    }
    output
}

#[test]
fn programs_print_what_python_prints() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
    let mut programs: Vec<_> = std::fs::read_dir(&folder)
        .expect("reads the fixtures")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "py"))
        .collect();
    programs.sort();
    assert!(!programs.is_empty());
    for program in programs {
        let expected =
            std::fs::read_to_string(program.with_extension("out")).expect("reads the output");
        let actual = run(&program);
        assert_eq!(actual, expected, "{}", program.display());
    }
}

fn evaluate(code: &str) -> Result<String, python_evaluator::Error> {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", code)?;
    let output = host.output.borrow().clone();
    Ok(output)
}

#[test]
fn errors_name_the_line_and_the_exception() {
    let error = evaluate("x = 1\ny = {}\ny['missing']\n").expect_err("a KeyError");
    assert_eq!(error.to_string(), "<test>:3: KeyError: 'missing'");
    assert!(!error.is_unsupported());
}

#[test]
fn unsupported_features_are_not_caught_by_except() {
    let error = evaluate("import os\ntry:\n    os.fork()\nexcept Exception:\n    pass\n")
        .expect_err("unsupported");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn writes_stay_in_memory() {
    let output = evaluate(
        "with open('iunctus-test-never-written.txt', 'w') as f:\n    f.write('hi')\nprint(open('iunctus-test-never-written.txt').read())\n",
    )
    .expect("runs");
    assert_eq!(output, "hi\n");
    assert!(
        !std::env::temp_dir()
            .join("iunctus-test-never-written.txt")
            .exists()
    );
}

#[test]
fn loops_that_never_end_stop() {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host);
    interpreter.step_limit = 10_000;
    let error = interpreter
        .run_text("<test>", "while True:\n    pass\n")
        .err()
        .expect("stops");
    assert!(error.is_unsupported());
}

#[test]
fn recursion_is_limited() {
    let error = evaluate("def f(n):\n    return f(n + 1)\nf(0)\n").expect_err("too deep");
    assert!(error.to_string().contains("RecursionError"), "{error}");
}

#[test]
fn yaml_follows_the_safe_loader() {
    let output = evaluate(
        "import yaml\nprint(yaml.safe_load('a: yes\\nb: 1e3\\nc: 1.0e+3\\nd: 010\\ne: 1:30\\nf: ~\\ng: [1, x]\\nh: &x {k: 1}\\ni: *x\\n'))\n",
    )
    .expect("runs");
    assert_eq!(
        output,
        "{'a': True, 'b': '1e3', 'c': 1000.0, 'd': 8, 'e': 90, 'f': None, 'g': [1, 'x'], 'h': {'k': 1}, 'i': {'k': 1}}\n"
    );
}

/// A host that answers each command with its words, and exit code 3 for `fail`.
struct CommandHost(LocalHost);

impl python_evaluator::Host for CommandHost {
    fn cwd(&self) -> std::path::PathBuf {
        self.0.cwd()
    }
    fn set_cwd(&self, path: &Path) {
        self.0.set_cwd(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.0.read(path)
    }
    fn stat(&self, path: &Path) -> Option<python_evaluator::Stat> {
        self.0.stat(path)
    }
    fn list(&self, path: &Path) -> std::io::Result<Vec<String>> {
        self.0.list(path)
    }
    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
        self.0.write(path, data, append)
    }
    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.make_dir(path)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove(path)
    }
    fn print(&self, text: &str) {
        self.0.print(text)
    }
    fn environment(&self) -> Vec<(String, String)> {
        self.0.environment()
    }
    fn run_command(
        &self,
        command: &python_evaluator::Command,
    ) -> Result<python_evaluator::CommandOutcome, python_evaluator::Error> {
        let words = command.argv.join(" ");
        Ok(python_evaluator::CommandOutcome {
            exit_code: if words.contains("fail") { 3 } else { 0 },
            stdout: format!("ran {words}\n").into_bytes(),
            stderr: Vec::new(),
        })
    }
}

#[test]
fn commands_run_where_the_host_runs_them() {
    let host = Rc::new(CommandHost(LocalHost::new(std::env::temp_dir())));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter
        .run_text(
            "<test>",
            "import os, subprocess\n\
             r = subprocess.run(['tool', '--version'], capture_output=True, text=True)\n\
             print(r.returncode, repr(r.stdout), r.stderr)\n\
             print(subprocess.check_output('tool a', shell=True), subprocess.call(['fail']))\n\
             try:\n    subprocess.run(['fail'], check=True)\n\
             except subprocess.CalledProcessError as error:\n    print(error.returncode, error.cmd, error)\n\
             print(os.system('fail'), os.popen('tool b').read().strip(), subprocess.getoutput('tool c'))\n",
        )
        .expect("runs");
    assert_eq!(
        *host.0.output.borrow(),
        "0 'ran tool --version\\n' \n\
         b'ran /bin/sh -c tool a\\n' 3\n\
         3 ['fail'] Command '['fail']' returned non-zero exit status 3.\n\
         768 ran /bin/sh -c tool b ran /bin/sh -c tool c\n"
    );
    let error = evaluate("import subprocess\nsubprocess.run(['tool'])\n").expect_err("no commands");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn missing_standard_modules_are_unsupported() {
    // Packages without a stand-in run in a container; tests/packages.rs covers them.
    let error = evaluate("import turtle\n").expect_err("no stand-in");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn integer_square_roots_do_not_overflow() {
    assert_eq!(
        evaluate("import math\nprint(math.isqrt(9223372036854775807), math.isqrt(10 ** 100), math.isqrt(0))\n")
            .expect("integer roots"),
        format!("3037000499 1{} 0\n", "0".repeat(50))
    );
    for code in ["math.isqrt()", "math.isqrt(1.5)"] {
        assert!(
            evaluate(&format!("import math\n{code}\n"))
                .expect_err("invalid argument")
                .is("TypeError")
        );
    }
    assert!(
        evaluate("import math\nmath.isqrt(-1)\n")
            .expect_err("negative root")
            .is("ValueError")
    );
}

#[test]
fn recursive_containers_have_bounded_representations() {
    assert_eq!(
        evaluate("a = []\na.append(a)\nd = {}\nd['self'] = d\nprint(a, d)\nprint([a, a])\n")
            .expect("recursive repr"),
        "[[...]] {'self': {...}}\n[[[...]], [[...]]]\n"
    );
}

#[test]
fn dataframe_indexes_reject_invalid_shapes_before_access() {
    for expression in [
        "pd.DataFrame({'x': [1]}).set_index(pd.Index(['a', 'b']))",
        "pd.MultiIndex.from_arrays([[1, 2], ['a']])",
        "pd.MultiIndex.from_arrays([[1], ['a']], names=['one'])",
        "pd.Series([1], index=['a', 'b'])",
        "pd.DataFrame([[1]], index=['a', 'b'])",
    ] {
        let error = evaluate(&format!("import pandas as pd\n{expression}\n"))
            .expect_err("invalid index shape");
        assert!(error.is("ValueError"), "{expression}: {error}");
    }
    assert_eq!(
        evaluate("import pandas as pd\nprint(pd.Series(index=['a', 'b']).isna().tolist())\n")
            .expect("empty series"),
        "[True, True]\n"
    );
}

#[test]
fn appending_preserves_disk_content_without_writing_it() {
    let folder = tempfile::tempdir().expect("a folder");
    let path = folder.path().join("existing.txt");
    std::fs::write(&path, "old").expect("initial content");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", "with open('existing.txt', 'a') as f:\n    f.write('new')\nwith open('existing.txt', 'a') as f:\n    f.write('again')\nprint(open('existing.txt').read())\n").expect("appends");
    assert_eq!(*host.output.borrow(), "oldnewagain\n");
    assert_eq!(std::fs::read_to_string(path).expect("disk content"), "old");
}

#[test]
fn truth_consumers_release_the_iterator_before_item_truthiness() {
    let output = evaluate(
        "class Item:\n    def __bool__(self):\n        next(it, None)\n        return True\nit = iter([Item(), 0])\nprint(any(it))\nit = iter([Item(), 0])\nprint(all(it))\n",
    ).expect("reentrant item truthiness");
    assert_eq!(output, "True\nTrue\n");
}

#[test]
fn generator_functions_suspend_effects_and_exceptions() {
    let output = evaluate(
        "events = []\ndef source():\n    events.append('first')\n    x = yield 1\n    events.append(x)\n    yield 2\n    raise ValueError('tail')\ng = source()\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\ntry:\n    next(g)\nexcept ValueError as error:\n    print(error)\nprint(next(g, 'done'))\n",
    ).expect("suspended generator");
    assert_eq!(output, "[]\n1 ['first']\n2 ['first', None]\ntail\ndone\n");
}

#[test]
fn generators_resume_nested_loop_flow_and_yield_from() {
    let output = evaluate(
        "def source():\n    for i in range(3):\n        for j in range(3):\n            if j == 0:\n                continue\n            if j == 2:\n                break\n            yield (i, j)\n        else:\n            yield 'wrong'\n    else:\n        yield 'outer'\n    k = 0\n    while k < 2:\n        k += 1\n        yield k\n    else:\n        yield from ['end', 'tail']\nprint(list(source()))\ndef forever():\n    while True:\n        yield True\ng = forever()\nprint(any(g), next(g))\nfor item in forever():\n    print(item)\n    break\ndef false_first():\n    yield False\n    raise ValueError('unconsumed')\nprint(all(false_first()))\n",
    ).expect("resumed loops");
    assert_eq!(
        output,
        "[(0, 1), (1, 1), (2, 1), 'outer', 1, 2, 'end', 'tail']\nTrue True\nTrue\nFalse\n"
    );
}

#[test]
fn generator_delegation_preserves_effect_timing() {
    let output = evaluate(
        "events = []\ndef inner():\n    events.append('first')\n    yield 1\n    events.append('second')\n    yield 2\ndef outer():\n    yield from inner()\n    events.append('end')\ng = outer()\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\nprint(next(g, None), events)\n",
    ).expect("incremental delegation");
    assert_eq!(
        output,
        "[]\n1 ['first']\n2 ['first', 'second']\nNone ['first', 'second', 'end']\n"
    );
}

#[test]
fn unsupported_generator_suspension_shapes_are_explicit() {
    for code in [
        "def source():\n    yield 1\n    x = 1 + (yield 2)\ng = source()\nnext(g)\nnext(g)\n",
        "def source():\n    try:\n        yield 1\n    finally:\n        pass\nnext(source())\n",
        "def source():\n    yield 1\nsource().send(None)\n",
    ] {
        let error = evaluate(code).expect_err("unsupported suspension");
        assert!(error.is_unsupported(), "{error}");
        assert!(
            error.to_string().contains("suspension") || error.to_string().contains("send"),
            "{error}"
        );
    }
    let output = evaluate("def source():\n    next(g)\n    yield 1\ng = source()\ntry:\n    next(g)\nexcept ValueError as error:\n    print(error)\n").expect("reentrant advancement is an exception");
    assert_eq!(output, "generator already executing\n");
}

#[test]
fn every_generator_consumer_refuses_reentrant_advancement() {
    for body in [
        "next(g)",
        "any(g)",
        "all(g)",
        "list(g)",
        "for item in g:\n        pass",
        "consume()",
        "list(itertools.islice(g, 1))",
    ] {
        let code = format!(
            "import itertools\ndef consume():\n    for item in g:\n        pass\ndef source():\n    {body}\n    yield 1\ng = source()\nnext(g)\n"
        );
        let error = evaluate(&code).expect_err("reentrant advancement");
        assert!(error.is("ValueError"), "{body}: {error}");
        assert!(error.to_string().contains("generator already executing"));
    }
}
