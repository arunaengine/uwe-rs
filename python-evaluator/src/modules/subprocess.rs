//! A stand-in for `subprocess`, and for `os.system` and `os.popen`. Commands run where the
//! host runs them, never on the machine of iunctus itself (decision 11); a Snakemake run turns
//! each into a container task. They give their exit code and output as in Python.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::cell::RefCell;

use super::function;
use crate::error::Error;
use crate::host::{Command, CommandOutcome};
use crate::interpreter::Interpreter;
use crate::types;
use crate::value::{Args, Native, Value};

/// `subprocess.PIPE`, `STDOUT` and `DEVNULL`.
const PIPE: i64 = -1;

/// Runs a command in a shell, as `os.system` and Snakemake's `shell()` do.
pub fn run_shell(interpreter: &mut Interpreter, text: &str) -> Result<CommandOutcome, Error> {
    let command = Command {
        argv: vec!["/bin/sh".into(), "-c".into(), text.to_owned()],
        cwd: interpreter.host.cwd(),
    };
    interpreter.host.run_command(&command)
}

/// The command of `args`: a list of words, or a text that a shell runs when `shell` is true
/// and that is the program otherwise.
fn command_of(
    interpreter: &mut Interpreter,
    args: &Value,
    shell: bool,
    options: &mut Args,
) -> Result<Command, Error> {
    for name in ["env", "input", "timeout"] {
        if options
            .take_keyword(name)
            .is_some_and(|value| !value.is_none())
        {
            return Err(Error::unsupported(format!("subprocess {name}")));
        }
    }
    let cwd = match options.take_keyword("cwd") {
        Some(value) if !value.is_none() => {
            let path = super::path_text(interpreter, &value)?;
            interpreter.host.resolve(&path)
        }
        _ => interpreter.host.cwd(),
    };
    let argv = match args.plain() {
        Value::Str(text) if shell => vec!["/bin/sh".into(), "-c".into(), text.to_string()],
        Value::Str(text) => vec![text.to_string()],
        _ => {
            let mut words = Vec::new();
            for word in interpreter.iterate(args)? {
                words.push(super::path_text(interpreter, &word)?);
            }
            if shell {
                vec!["/bin/sh".into(), "-c".into(), words.join(" ")]
            } else {
                words
            }
        }
    };
    Ok(Command { argv, cwd })
}

/// Output as text when the call asks for text, else as bytes.
fn output(bytes: &[u8], text: bool) -> Value {
    if text {
        Value::str(String::from_utf8_lossy(bytes).as_ref())
    } else {
        Value::Bytes(bytes.into())
    }
}

/// Whether a call asks for text output: `text=True`, `universal_newlines=True` or an encoding.
fn text_mode(interpreter: &mut Interpreter, args: &mut Args) -> Result<bool, Error> {
    let mut text = false;
    for name in ["text", "universal_newlines"] {
        if let Some(value) = args.take_keyword(name) {
            text |= interpreter.truthy(&value)?;
        }
    }
    if let Some(encoding) = args.take_keyword("encoding") {
        text |= !encoding.is_none();
    }
    args.take_keyword("errors");
    Ok(text)
}

fn flag(interpreter: &mut Interpreter, args: &mut Args, name: &str) -> Result<bool, Error> {
    match args.take_keyword(name) {
        Some(value) => interpreter.truthy(&value),
        None => Ok(false),
    }
}

/// `subprocess.CalledProcessError` for a command that ended with another exit code than 0.
pub fn called_process_error(
    interpreter: &mut Interpreter,
    command: &Value,
    outcome: &CommandOutcome,
    text: bool,
) -> Result<Error, Error> {
    let class = types::library_exception("CalledProcessError", "subprocess", "SubprocessError");
    let message = format!(
        "Command '{}' returned non-zero exit status {}.",
        interpreter.to_str(command)?,
        outcome.exit_code
    );
    let error = crate::error::exception(&class, vec![Value::str(message)]);
    if let Value::Instance(instance) = &error {
        let mut dict = instance.dict.borrow_mut();
        dict.insert(
            "returncode".into(),
            Value::Int(i64::from(outcome.exit_code)),
        );
        dict.insert("cmd".into(), command.clone());
        dict.insert("output".into(), output(&outcome.stdout, text));
        dict.insert("stdout".into(), output(&outcome.stdout, text));
        dict.insert("stderr".into(), output(&outcome.stderr, text));
    }
    Ok(Error::from_value(error))
}

/// What `subprocess.run` gives: the command, its exit code and the output it captured.
struct CompletedProcess {
    args: Value,
    returncode: i32,
    stdout: Value,
    stderr: Value,
}

impl Native for CompletedProcess {
    fn type_name(&self) -> &str {
        "CompletedProcess"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match name {
            "args" => self.args.clone(),
            "returncode" => Value::Int(i64::from(self.returncode)),
            "stdout" => self.stdout.clone(),
            "stderr" => self.stderr.clone(),
            "check_returncode" => {
                let (returncode, args) = (self.returncode, self.args.clone());
                Value::builtin("check_returncode", move |interpreter, _| {
                    if returncode == 0 {
                        return Ok(Value::None);
                    }
                    let outcome = CommandOutcome {
                        exit_code: returncode,
                        ..CommandOutcome::default()
                    };
                    Err(called_process_error(interpreter, &args, &outcome, true)?)
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// `subprocess.run(args, capture_output=False, check=False, shell=False, text=False, ...)`.
fn run(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let command_value = args.take(0, "args").unwrap_or(Value::None);
    let shell = flag(interpreter, &mut args, "shell")?;
    let capture = flag(interpreter, &mut args, "capture_output")?;
    let check = flag(interpreter, &mut args, "check")?;
    let text = text_mode(interpreter, &mut args)?;
    let piped = |value: Option<Value>| value.and_then(|value| value.as_int()) == Some(PIPE);
    let capture_stdout = capture || piped(args.take_keyword("stdout"));
    let capture_stderr = capture || piped(args.take_keyword("stderr"));
    let command = command_of(interpreter, &command_value, shell, &mut args)?;
    let outcome = interpreter.host.run_command(&command)?;
    if check && outcome.exit_code != 0 {
        return Err(called_process_error(
            interpreter,
            &command_value,
            &outcome,
            text,
        )?);
    }
    Ok(Value::native(CompletedProcess {
        args: command_value,
        returncode: outcome.exit_code,
        stdout: if capture_stdout {
            output(&outcome.stdout, text)
        } else {
            Value::None
        },
        stderr: if capture_stderr {
            output(&outcome.stderr, text)
        } else {
            Value::None
        },
    }))
}

/// `subprocess.check_output(args, shell=False, text=False, ...)`.
fn check_output(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let command_value = args.take(0, "args").unwrap_or(Value::None);
    let shell = flag(interpreter, &mut args, "shell")?;
    let text = text_mode(interpreter, &mut args)?;
    let command = command_of(interpreter, &command_value, shell, &mut args)?;
    let outcome = interpreter.host.run_command(&command)?;
    if outcome.exit_code != 0 {
        return Err(called_process_error(
            interpreter,
            &command_value,
            &outcome,
            text,
        )?);
    }
    Ok(output(&outcome.stdout, text))
}

/// `subprocess.call` and `check_call`: the exit code, or an error for `check_call`.
fn call(interpreter: &mut Interpreter, mut args: Args, check: bool) -> Result<Value, Error> {
    let command_value = args.take(0, "args").unwrap_or(Value::None);
    let shell = flag(interpreter, &mut args, "shell")?;
    let command = command_of(interpreter, &command_value, shell, &mut args)?;
    let outcome = interpreter.host.run_command(&command)?;
    if check && outcome.exit_code != 0 {
        return Err(called_process_error(
            interpreter,
            &command_value,
            &outcome,
            false,
        )?);
    }
    Ok(Value::Int(i64::from(outcome.exit_code)))
}

/// `subprocess.getstatusoutput(cmd)`: the exit code and the output without its last newline.
fn status_output(interpreter: &mut Interpreter, args: &Args) -> Result<(i32, String), Error> {
    let text = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
    let outcome = run_shell(interpreter, &text)?;
    let mut output = String::from_utf8_lossy(&outcome.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&outcome.stderr));
    if output.ends_with('\n') {
        output.pop();
    }
    Ok((outcome.exit_code, output))
}

/// `subprocess.Popen`: the command runs at once, and the object gives what it did.
struct Popen {
    args: Value,
    outcome: CommandOutcome,
    text: bool,
    captured: bool,
    read: RefCell<bool>,
}

impl Native for Popen {
    fn type_name(&self) -> &str {
        "Popen"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = this.clone();
        let returncode = Value::Int(i64::from(self.outcome.exit_code));
        Ok(Some(match name {
            "args" => self.args.clone(),
            "returncode" | "pid" => returncode,
            "stdout" if self.captured => {
                let text = String::from_utf8_lossy(&self.outcome.stdout).into_owned();
                let io = interpreter.import_module("io")?;
                let string_io = interpreter.get_attribute(&io, "StringIO")?;
                interpreter.call(&string_io, Args::new(vec![Value::str(text)]))?
            }
            "stdout" | "stderr" | "stdin" => Value::None,
            "wait" | "poll" => Value::builtin(name, move |_, _| Ok(returncode.clone())),
            "communicate" => Value::builtin("communicate", move |_, _| {
                let Some(process) = this.as_native::<Popen>() else {
                    return Err(Error::type_error("not a Popen"));
                };
                let first = !process.read.replace(true);
                let stdout = if process.captured && first {
                    output(&process.outcome.stdout, process.text)
                } else {
                    Value::None
                };
                Ok(Value::tuple(vec![stdout, Value::None]))
            }),
            "kill" | "terminate" => Value::builtin(name, |_, _| Ok(Value::None)),
            "__enter__" => Value::builtin(name, move |_, _| Ok(this.clone())),
            "__exit__" => Value::builtin(name, |_, _| Ok(Value::Bool(false))),
            _ => return Ok(None),
        }))
    }
}

fn popen(interpreter: &mut Interpreter, mut args: Args) -> Result<Value, Error> {
    let command_value = args.take(0, "args").unwrap_or(Value::None);
    let shell = flag(interpreter, &mut args, "shell")?;
    let text = text_mode(interpreter, &mut args)?;
    let captured = args.take_keyword("stdout").and_then(|value| value.as_int()) == Some(PIPE);
    let command = command_of(interpreter, &command_value, shell, &mut args)?;
    let outcome = interpreter.host.run_command(&command)?;
    Ok(Value::native(Popen {
        args: command_value,
        outcome,
        text,
        captured,
        read: RefCell::new(false),
    }))
}

pub fn subprocess() -> Vec<(&'static str, Value)> {
    let base = types::library_exception("SubprocessError", "subprocess", "Exception");
    vec![
        ("PIPE", Value::Int(PIPE)),
        ("STDOUT", Value::Int(-2)),
        ("DEVNULL", Value::Int(-3)),
        ("SubprocessError", Value::Class(base)),
        (
            "CalledProcessError",
            Value::Class(types::library_exception(
                "CalledProcessError",
                "subprocess",
                "SubprocessError",
            )),
        ),
        (
            "TimeoutExpired",
            Value::Class(types::library_exception(
                "TimeoutExpired",
                "subprocess",
                "SubprocessError",
            )),
        ),
        ("run", function("run", run)),
        ("check_output", function("check_output", check_output)),
        (
            "call",
            function("call", |interpreter, args| call(interpreter, args, false)),
        ),
        (
            "check_call",
            function("check_call", |interpreter, args| {
                call(interpreter, args, true)
            }),
        ),
        ("Popen", function("Popen", popen)),
        (
            "getoutput",
            function("getoutput", |interpreter, args| {
                Ok(Value::str(status_output(interpreter, &args)?.1))
            }),
        ),
        (
            "getstatusoutput",
            function("getstatusoutput", |interpreter, args| {
                let (status, output) = status_output(interpreter, &args)?;
                Ok(Value::tuple(vec![
                    Value::Int(i64::from(status)),
                    Value::str(output),
                ]))
            }),
        ),
    ]
}

/// `os.system(command)`: the wait status, which is the exit code times 256.
pub fn system(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let text = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
    let outcome = run_shell(interpreter, &text)?;
    Ok(Value::Int(i64::from(outcome.exit_code) << 8))
}

/// `os.popen(command)`: a file with the output of the command.
pub fn os_popen(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let text = interpreter.to_str(&args.get(0).cloned().unwrap_or(Value::None))?;
    let outcome = run_shell(interpreter, &text)?;
    let io = interpreter.import_module("io")?;
    let string_io = interpreter.get_attribute(&io, "StringIO")?;
    let output = String::from_utf8_lossy(&outcome.stdout).into_owned();
    interpreter.call(&string_io, Args::new(vec![Value::str(output)]))
}
