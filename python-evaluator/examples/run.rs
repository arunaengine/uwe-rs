//! Runs a Python file with the evaluator and prints its output, like `python3 file.py`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: run <file.py>");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).expect("reads the file");
    let cwd = std::env::current_dir().expect("a working directory");
    let host = Rc::new(LocalHost::new(cwd));
    let mut interpreter = Interpreter::new(host.clone());
    let result = interpreter.run_text(&path, &text);
    print!("{}", host.output.borrow());
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(if error.is_unsupported() { 33 } else { 1 });
    }
}
