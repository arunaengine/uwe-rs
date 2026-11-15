//! Renders a tool's command and configfiles for a job: the template tree on Galaxy values,
//! then Galaxy's joining of the command lines.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::rc::Rc;

use galaxy_parser::tool::{Template, Tool};
use python_evaluator::{Args, Error as Python, Host, Interpreter, Scope, Stat, Value};

use crate::parser::{self, FOLLOW, Node};
use crate::values::{self, Job};
use crate::{Error, outputs};

/// A rendered job: the command line, the configfile texts by name, and the created outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub command: Option<String>,
    pub configfiles: Vec<(String, String)>,
    pub created: Vec<String>,
}

/// Renders `tool` for `job`. The same tool, values and files always give the same text.
pub fn render(tool: &Tool, job: &Job) -> Result<Rendered, Error> {
    let named = |part: &str, error: Error| Error::Tool {
        tool: tool.id.clone(),
        part: part.to_owned(),
        source: Box::new(error),
    };
    let mut interpreter = interpreter();
    let created =
        outputs::created(tool, job, &mut interpreter).map_err(|error| named("outputs", error))?;
    let mut context = vec![(FOLLOW.to_owned(), Value::builtin(FOLLOW, follow))];
    context.extend(values::wrapped(tool, job).map_err(|error| named("inputs", error))?);
    for output in &tool.outputs {
        let path = job.outputs.get(&output.name).map(String::as_str);
        let made = created.contains(&output.name);
        if made && path.is_none() {
            return Err(named(
                "outputs",
                Error::Input(format!("no path for the output {}", output.name)),
            ));
        }
        let value = values::output(path.filter(|_| made), output.format.as_deref());
        context.push((output.name.clone(), value));
    }
    for (name, path) in &job.configfiles {
        context.push((name.clone(), Value::str(path.as_str())));
    }
    let directory = Value::str(job.tool_directory.as_str());
    context.push(("__tool_directory__".to_owned(), directory));
    let mut configfiles = Vec::new();
    for configfile in &tool.configfiles {
        let text = template(&configfile.template, &mut interpreter, &context)
            .map_err(|error| named(&format!("configfile {}", configfile.name), error))?;
        configfiles.push((configfile.name.clone(), text));
    }
    let command = match &tool.command {
        Some(command) => Some(command_line(
            &template(command, &mut interpreter, &context)
                .map_err(|error| named("command", error))?,
        )),
        None => None,
    };
    Ok(Rendered {
        command,
        configfiles,
        created,
    })
}

/// Galaxy's `_build_command_line`: each line stripped, and the lines joined with spaces.
pub fn command_line(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    lines
        .join("\n")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_owned()
}

/// An interpreter whose host refuses files and commands, so rendering stays pure.
pub fn interpreter() -> Interpreter {
    Interpreter::new(Rc::new(Closed))
}

/// Renders the template of a tool part in its own scope over the shared `context`, so that its
/// `#set` stays its own, as each Galaxy template is a separate Cheetah template.
fn template(
    template: &Template,
    interpreter: &mut Interpreter,
    context: &[(String, Value)],
) -> Result<String, Error> {
    let shift = |error: Error| error.shifted(template.line.saturating_sub(1));
    let nodes = parser::parse(&template.text).map_err(shift)?;
    let globals = interpreter.new_globals("__cheetah__", None);
    for (name, value) in context {
        globals.set(name, value.clone());
    }
    let mut out = String::new();
    nodes_into(&nodes, interpreter, &globals, &mut out).map_err(shift)?;
    Ok(out)
}

fn nodes_into(
    nodes: &[Node],
    interpreter: &mut Interpreter,
    globals: &Rc<Scope>,
    out: &mut String,
) -> Result<(), Error> {
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Value { expression, line } => {
                let value = evaluate(interpreter, expression, globals, *line)?;
                // Cheetah's default filter prints None as nothing.
                if !value.is_none() {
                    let text = interpreter.to_str(&value).map_err(|source| Error::Python {
                        line: *line,
                        source,
                    })?;
                    out.push_str(&text);
                }
            }
            Node::If {
                branches,
                otherwise,
                line,
            } => {
                let mut chosen = otherwise;
                for (condition, body) in branches {
                    let value = evaluate(interpreter, condition, globals, *line)?;
                    let holds = interpreter.truthy(&value).map_err(|source| Error::Python {
                        line: *line,
                        source,
                    })?;
                    if holds {
                        chosen = body;
                        break;
                    }
                }
                nodes_into(chosen, interpreter, globals, out)?;
            }
            Node::Set { statement, line } => interpreter
                .exec_text("<template>", statement, globals)
                .map_err(|source| Error::Python {
                    line: *line,
                    source,
                })?,
        }
    }
    Ok(())
}

fn evaluate(
    interpreter: &mut Interpreter,
    expression: &str,
    globals: &Rc<Scope>,
    line: usize,
) -> Result<Value, Error> {
    interpreter
        .eval_text(expression, globals)
        .map_err(|source| Error::Python { line, source })
}

/// Cheetah's NameMapper: `(object, "a.b", autocall)` follows each name as a dict key or an
/// attribute, and calls methods that are not followed by parentheses.
fn follow(interpreter: &mut Interpreter, args: Args) -> Result<Value, Python> {
    let [object, path, autocall] = args.positional.as_slice() else {
        return Err(Python::type_error(
            "the NameMapper lookup takes three arguments",
        ));
    };
    let autocall = interpreter.truthy(autocall)?;
    let mut value = called(interpreter, object.clone(), autocall)?;
    for name in path
        .as_str()
        .unwrap_or_default()
        .split('.')
        .filter(|name| !name.is_empty())
    {
        let next = match value.plain() {
            Value::Dict(dict) => dict.borrow().get(name).cloned(),
            _ => None,
        };
        let next = match next {
            Some(next) => next,
            None => interpreter.get_attribute(&value, name)?,
        };
        value = called(interpreter, next, autocall)?;
    }
    Ok(value)
}

fn called(interpreter: &mut Interpreter, value: Value, autocall: bool) -> Result<Value, Python> {
    if autocall && !matches!(value.plain(), Value::Class(_)) && interpreter.callable(&value) {
        return interpreter.call(&value, Args::default());
    }
    Ok(value)
}

/// A host without files, output or commands: templates and filters only compute text.
struct Closed;

impl Host for Closed {
    fn cwd(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn set_cwd(&self, _: &Path) {}

    fn read(&self, _: &Path) -> std::io::Result<Vec<u8>> {
        Err(refused())
    }

    fn stat(&self, _: &Path) -> Option<Stat> {
        None
    }

    fn list(&self, _: &Path) -> std::io::Result<Vec<String>> {
        Err(refused())
    }

    fn write(&self, _: &Path, _: &[u8], _: bool) -> std::io::Result<()> {
        Err(refused())
    }

    fn make_dir(&self, _: &Path) -> std::io::Result<()> {
        Err(refused())
    }

    fn remove(&self, _: &Path) -> std::io::Result<()> {
        Err(refused())
    }

    fn print(&self, _: &str) {}

    fn environment(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

fn refused() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "templates have no file access",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(xml: &str) -> Tool {
        galaxy_parser::tool::parse(xml, "t.xml", &mut |_| unreachable!("no imports"))
            .expect("a tool")
    }

    #[test]
    fn joins_stripped_lines() {
        assert_eq!(command_line("\n  a \n\n b\r\n  "), "a  b");
    }

    #[test]
    fn follows_lookup_chunks() {
        let tool = tool(
            "<tool id=\"t\"><command>$v.strip.upper() $rep[0].n ${v.split(',')[0].upper()} \
             $v.split(',')[0].upper</command><inputs><param name=\"v\" type=\"text\" value=\"a,b\"/>\
             <repeat name=\"rep\"><param name=\"n\" type=\"integer\"/></repeat></inputs></tool>",
        );
        let job = Job {
            parameters: serde_json::json!({"rep": [{"n": 7}]}),
            ..Job::default()
        };
        let rendered = render(&tool, &job).expect("renders");
        // Cheetah ends a placeholder after a call that a subscript follows; the rest is text.
        assert_eq!(
            rendered.command.as_deref(),
            Some("A,B 7 A ['a', 'b'][0].upper")
        );
    }

    #[test]
    fn templates_keep_assignments() {
        let tool = tool(
            "<tool id=\"t\"><command>$mode $cfg</command><configfiles>\
             <configfile name=\"cfg\">#set $mode = 'config'\n$mode</configfile></configfiles>\
             <inputs><param name=\"mode\" type=\"text\" value=\"cmd\"/></inputs></tool>",
        );
        let mut job = Job::default();
        job.configfiles.insert("cfg".to_owned(), "c.txt".to_owned());
        let rendered = render(&tool, &job).expect("renders");
        assert_eq!(
            rendered.configfiles,
            [("cfg".to_owned(), "config".to_owned())]
        );
        assert_eq!(
            rendered.command.as_deref(),
            Some("cmd c.txt"),
            "the #set stays in its file"
        );
    }

    #[test]
    fn autocalls_bare_methods() {
        let tool = tool(
            "<tool id=\"t\"><command>$v.upper ${v.upper()} $sec.inner</command><inputs>\
             <param name=\"v\" type=\"text\" value=\"ab\"/><section name=\"sec\">\
             <param name=\"inner\" type=\"integer\" value=\"3\"/></section></inputs></tool>",
        );
        let rendered = render(&tool, &Job::default()).expect("renders");
        assert_eq!(rendered.command.as_deref(), Some("AB AB 3"));
    }

    #[test]
    fn errors_name_line() {
        let unknown = tool("<tool id=\"t\">\n<inputs/>\n<command>a\n$missing</command></tool>");
        let error = render(&unknown, &Job::default()).expect_err("an unknown name");
        let text = error.to_string();
        assert!(text.contains("tool t") && text.contains("line 4"), "{text}");
        let looped = tool("<tool id=\"t\"><command>#for $i in [1]:\n$i\n#end for</command></tool>");
        let error = render(&looped, &Job::default()).expect_err("a later tier");
        assert!(error.to_string().contains("#for"), "{error}");
        let filed =
            tool("<tool id=\"t\"><command>${open('/etc/hostname').read()}</command></tool>");
        assert!(
            render(&filed, &Job::default()).is_err(),
            "templates cannot read files"
        );
    }
}
