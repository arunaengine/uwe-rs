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
fn missing_packages_are_errors_and_missing_standard_modules_are_unsupported() {
    let output = evaluate(
        "try:\n    import some_package_nobody_installed\nexcept ModuleNotFoundError as error:\n    print(error)\n",
    )
    .expect("runs");
    assert_eq!(output, "No module named 'some_package_nobody_installed'\n");
    let error = evaluate("import turtle\n").expect_err("no stand-in");
    assert!(error.is_unsupported(), "{error}");
}
