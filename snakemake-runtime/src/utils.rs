//! The `snakemake` package as Snakefiles import it: `snakemake.utils` with `min_version`
//! and `validate`, `snakemake.io`, `snakemake.exceptions` and `snakemake.logging`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use python_evaluator::interpreter::ModuleFactory;
use python_evaluator::modules::module;
use python_evaluator::{Args, Error, Interpreter, Native, Scope, Value};

use crate::functions;
use crate::workflow::{State, workflow_error};

/// The Snakemake version that iunctus follows.
pub const VERSION: &str = "9.27.0";

fn version_parts(text: &str) -> Vec<i64> {
    text.trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

fn min_version(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    args.expect("min_version", 1, 1)?;
    let wanted = interpreter.to_str(&args.positional[0])?;
    let (mut have, mut want) = (version_parts(VERSION), version_parts(&wanted));
    let length = have.len().max(want.len());
    have.resize(length, 0);
    want.resize(length, 0);
    if have < want {
        return Err(workflow_error(format!(
            "Expecting Snakemake version {wanted} or higher (you are currently using {VERSION})."
        )));
    }
    Ok(Value::None)
}

/// `validate(data, schema, set_default=True)`.
fn validate(
    interpreter: &mut Interpreter,
    state: &Rc<RefCell<State>>,
    mut args: Args,
) -> Result<Value, Error> {
    let set_default = match args.take(2, "set_default") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    let data = args
        .take(0, "data")
        .ok_or_else(|| Error::type_error("validate() missing data"))?;
    let schema = args
        .take(1, "schema")
        .ok_or_else(|| Error::type_error("validate() missing schema"))?;
    let (skip, base) = {
        let state = state.borrow();
        (
            state
                .modifiers
                .iter()
                .any(|modifier| modifier.skip_validation),
            state.current_basedir(),
        )
    };
    if skip {
        return Ok(Value::None);
    }
    let schema_path = python_evaluator::modules::path_text(interpreter, &schema)?;
    let (schema, file) = crate::schema::load_schema(interpreter, &schema_path, &base)?;
    if matches!(data.plain(), Value::Dict(_)) {
        if let Err(message) =
            crate::schema::validate_record(interpreter, &data, &schema, &file, set_default)?
        {
            return Err(workflow_error(format!(
                "Error validating config file.\n{message}"
            )));
        }
        return Ok(Value::None);
    }
    if let Some(records) = python_evaluator::pandas::records(interpreter, &data)? {
        let mut validated = Vec::with_capacity(records.len());
        for (index, record) in records.into_iter().enumerate() {
            if let Err(message) =
                crate::schema::validate_record(interpreter, &record, &schema, &file, set_default)?
            {
                return Err(workflow_error(format!(
                    "Error validating row {index} of data frame.\n{message}"
                )));
            }
            validated.push(record);
        }
        if set_default {
            python_evaluator::pandas::fill_from_records(interpreter, &data, &validated)?;
        }
        return Ok(Value::None);
    }
    Ok(Value::None)
}

fn utils_module(interpreter: &mut Interpreter, state: Rc<RefCell<State>>) -> Result<Value, Error> {
    let validate_state = state.clone();
    let _ = interpreter;
    Ok(module(
        "snakemake.utils",
        vec![
            ("min_version", Value::builtin("min_version", min_version)),
            // Snakemake 9 keeps these in `snakemake.utils`, not in `snakemake.io`.
            (
                "_load_configfile",
                Value::builtin("_load_configfile", |interpreter, args| {
                    let path = args.get(0).cloned().unwrap_or(Value::None);
                    let path = python_evaluator::modules::path_text(interpreter, &path)?;
                    crate::config::load_config(interpreter, &path)
                }),
            ),
            (
                "Namedlist",
                Value::Class(python_evaluator::types::native_class("Namedlist")),
            ),
            (
                "Wildcards",
                Value::Class(python_evaluator::types::native_class("Wildcards")),
            ),
            (
                "validate",
                Value::builtin("validate", move |interpreter, args| {
                    validate(interpreter, &validate_state, args)
                }),
            ),
            (
                "update_config",
                Value::builtin("update_config", |interpreter, args| {
                    args.expect("update_config", 2, 2)?;
                    crate::config::update(interpreter, &args.positional[0], &args.positional[1])?;
                    Ok(Value::None)
                }),
            ),
            (
                "available_cpu_count",
                Value::builtin("available_cpu_count", |_, _| {
                    Ok(Value::Int(
                        std::thread::available_parallelism().map_or(1, |count| count.get()) as i64,
                    ))
                }),
            ),
            (
                "format",
                Value::builtin("format", |interpreter, mut args| {
                    let template = args
                        .positional
                        .first()
                        .cloned()
                        .ok_or_else(|| Error::type_error("format() missing pattern"))?;
                    let template = interpreter.to_str(&template)?;
                    args.positional.clear();
                    Ok(Value::str(crate::shell::format_command(
                        interpreter,
                        &template,
                        &args,
                        None,
                    )?))
                }),
            ),
            (
                "makedirs",
                Value::builtin("makedirs", |interpreter, args| {
                    let os = interpreter.import_module("os")?;
                    let makedirs = interpreter.get_attribute(&os, "makedirs")?;
                    let mut call = Args::new(args.positional.clone());
                    call.keywords.push(("exist_ok".into(), Value::Bool(true)));
                    interpreter.call(&makedirs, call)
                }),
            ),
            (
                "listfiles",
                Value::builtin("listfiles", |interpreter, args| {
                    let found = functions::glob_wildcards(interpreter, args.clone())?;
                    let _ = found;
                    Err(Error::unsupported("snakemake.utils.listfiles"))
                }),
            ),
            (
                "Paramspace",
                Value::builtin("Paramspace", crate::paramspace::new),
            ),
            (
                "report",
                Value::builtin("report", |_, _| {
                    Err(Error::unsupported("snakemake.utils.report"))
                }),
            ),
            (
                "R",
                Value::builtin("R", |_, _| Err(Error::unsupported("snakemake.utils.R"))),
            ),
            (
                "linecount",
                Value::builtin("linecount", |interpreter, args| {
                    let path =
                        python_evaluator::modules::path_text(interpreter, &args.positional[0])?;
                    let resolved = interpreter.host.resolve(&path);
                    let data = interpreter
                        .host
                        .read(&resolved)
                        .map_err(|error| Error::os_error(&error, &path))?;
                    Ok(Value::Int(
                        data.iter().filter(|byte| **byte == b'\n').count() as i64,
                    ))
                }),
            ),
        ],
    ))
}

fn exceptions_module() -> Value {
    let workflow = python_evaluator::types::library_exception(
        "WorkflowError",
        "snakemake.exceptions",
        "Exception",
    );
    let _ = workflow;
    let names = [
        ("WorkflowError", "Exception"),
        ("RuleException", "Exception"),
        ("InputFunctionException", "WorkflowError"),
        ("MissingInputException", "RuleException"),
        ("IncompleteCheckpointException", "Exception"),
        ("CreateRuleException", "RuleException"),
        ("LookupError", "WorkflowError"),
    ];
    let entries: Vec<(&str, Value)> = names
        .into_iter()
        .map(|(name, base)| {
            (
                name,
                Value::Class(python_evaluator::types::library_exception(
                    name,
                    "snakemake.exceptions",
                    base,
                )),
            )
        })
        .collect();
    module("snakemake.exceptions", entries)
}

fn io_module(globals: &Rc<Scope>) -> Value {
    let names = [
        "expand",
        "glob_wildcards",
        "temp",
        "temporary",
        "protected",
        "directory",
        "ancient",
        "touch",
        "unpack",
        "multiext",
        "pipe",
        "service",
        "local",
        "ensure",
        "report",
        "repeat",
        "flag",
    ];
    let entries: Vec<(&str, Value)> = names
        .iter()
        .filter_map(|name| globals.get(name).map(|value| (*name, value)))
        .collect();
    module("snakemake.io", entries)
}

/// The remote providers of Snakemake 7, whose modules Snakemake 9 still has.
const REMOTE_PROVIDERS: &[&str] = &[
    "AzBlob", "dropbox", "EGA", "FTP", "gfal", "gridftp", "GS", "HTTP", "iRODS", "NCBI", "S3",
    "S3Mocked", "SFTP", "webdav", "XRootD", "zenodo",
];

/// A remote provider class of Snakemake 7: making one fails as in Snakemake 9.
fn remote_provider() -> Value {
    Value::builtin("RemoteProvider", |_, _| {
        Err(Error::new(
            "NotImplementedError",
            "Remote providers have been replaced by Snakemake storage plugins. Please use the corresponding storage plugin instead (snakemake-storage-plugin-*).",
        ))
    })
}

/// `snakemake.dag.toposort(graph)`: the nodes of a graph, given as each node's predecessors,
/// in layers whose nodes depend only on those of earlier layers, as `graphlib` makes them.
fn toposort(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let graph = args.get(0).cloned().unwrap_or(Value::None);
    let mut nodes: Vec<Value> = Vec::new();
    let mut predecessors: Vec<Vec<Value>> = Vec::new();
    let add = |node: &Value, nodes: &mut Vec<Value>, predecessors: &mut Vec<Vec<Value>>| match nodes
        .iter()
        .position(|known| python_evaluator::value::key_equal(known, node))
    {
        Some(position) => position,
        None => {
            nodes.push(node.clone());
            predecessors.push(Vec::new());
            nodes.len() - 1
        }
    };
    for (node, before) in interpreter.mapping_items(&graph)? {
        let position = add(&node, &mut nodes, &mut predecessors);
        for item in interpreter.iterate(&before)? {
            add(&item, &mut nodes, &mut predecessors);
            predecessors[position].push(item);
        }
    }
    let mut done = vec![false; nodes.len()];
    let mut layers = Vec::new();
    while done.iter().any(|finished| !finished) {
        let ready: Vec<usize> = (0..nodes.len())
            .filter(|position| !done[*position])
            .filter(|position| {
                predecessors[*position].iter().all(|before| {
                    nodes
                        .iter()
                        .position(|known| python_evaluator::value::key_equal(known, before))
                        .is_some_and(|index| done[index])
                })
            })
            .collect();
        if ready.is_empty() {
            return Err(Error::new("CycleError", "nodes are in a cycle"));
        }
        let mut layer = indexmap::IndexSet::new();
        for position in ready {
            done[position] = true;
            layer.insert(python_evaluator::Key::new(nodes[position].clone())?);
        }
        layers.push(Value::set(layer));
    }
    Ok(Value::list(layers))
}

/// `ExecMode`: the modes `DEFAULT`, `SUBPROCESS` and `REMOTE`, each one object.
struct ExecMode(Vec<(&'static str, Value)>);

/// A member of `ExecMode`.
struct ExecModeMember(&'static str);

impl ExecMode {
    fn value() -> Value {
        Value::native(ExecMode(
            ["DEFAULT", "SUBPROCESS", "REMOTE"]
                .into_iter()
                .map(|name| (name, Value::native(ExecModeMember(name))))
                .collect(),
        ))
    }
}

impl Native for ExecMode {
    fn type_name(&self) -> &str {
        "EnumType"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(self
            .0
            .iter()
            .find(|(member, _)| *member == name)
            .map(|(_, value)| value.clone()))
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        Some(value.as_native::<ExecModeMember>().is_some())
    }
}

impl Native for ExecModeMember {
    fn type_name(&self) -> &str {
        "ExecMode"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(match name {
            "name" => Some(Value::str(self.0)),
            "value" => Some(Value::Int(match self.0 {
                "DEFAULT" => 0,
                "SUBPROCESS" => 1,
                _ => 2,
            })),
            _ => None,
        })
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        Ok(format!(
            "<ExecMode.{}: {}>",
            self.0,
            match self.0 {
                "DEFAULT" => 0,
                "SUBPROCESS" => 1,
                _ => 2,
            }
        ))
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        Ok(Some(format!("ExecMode.{}", self.0)))
    }
}

/// Registers the `snakemake` modules with an interpreter.
pub fn register(interpreter: &mut Interpreter, state: &Rc<RefCell<State>>, globals: &Rc<Scope>) {
    let factory = |make: ModuleFactory| make;
    let utils_state = state.clone();
    interpreter.host_modules.insert(
        "snakemake.utils".into(),
        factory(Rc::new(move |interpreter| {
            utils_module(interpreter, utils_state.clone())
        })),
    );
    interpreter.host_modules.insert(
        "snakemake.exceptions".into(),
        factory(Rc::new(|_| Ok(exceptions_module()))),
    );
    let io_globals = globals.clone();
    interpreter.host_modules.insert(
        "snakemake.io".into(),
        factory(Rc::new(move |_| Ok(io_module(&io_globals)))),
    );
    interpreter.host_modules.insert(
        "snakemake.logging".into(),
        factory(Rc::new(|_| {
            Ok(module(
                "snakemake.logging",
                vec![(
                    "logger",
                    python_evaluator::modules::logger("snakemake.logging"),
                )],
            ))
        })),
    );
    let shell_globals = globals.clone();
    interpreter.host_modules.insert(
        "snakemake.shell".into(),
        factory(Rc::new(move |_| {
            Ok(module(
                "snakemake.shell",
                vec![("shell", shell_globals.get("shell").unwrap_or(Value::None))],
            ))
        })),
    );
    let workflow_globals = globals.clone();
    interpreter.host_modules.insert(
        "snakemake.workflow".into(),
        factory(Rc::new(move |_| {
            let names = ["workflow", "rules", "checkpoints", "expand", "config"];
            let mut entries: Vec<(&str, Value)> = names
                .iter()
                .filter_map(|name| workflow_globals.get(name).map(|value| (*name, value)))
                .collect();
            entries.push((
                "Workflow",
                Value::Class(python_evaluator::types::native_class("Workflow")),
            ));
            Ok(module("snakemake.workflow", entries))
        })),
    );
    // Snakemake 8 replaced remote providers by storage plugins; the modules remain, and
    // making a provider fails.
    for name in REMOTE_PROVIDERS {
        interpreter.host_modules.insert(
            format!("snakemake.remote.{name}"),
            factory(Rc::new(|_| {
                Ok(module(
                    "snakemake.remote",
                    vec![("RemoteProvider", remote_provider())],
                ))
            })),
        );
    }
    interpreter.host_modules.insert(
        "snakemake.remote".into(),
        factory(Rc::new(|_| {
            Ok(module(
                "snakemake.remote",
                vec![("RemoteProviderBase", remote_provider())],
            ))
        })),
    );
    // `ExecMode` of the interface of executor plugins, which workflows compare with to learn
    // whether they are the main process.
    let exec_mode = ExecMode::value();
    interpreter.host_modules.insert(
        "snakemake_interface_executor_plugins.settings".into(),
        factory(Rc::new(move |_| {
            Ok(module(
                "snakemake_interface_executor_plugins.settings",
                vec![("ExecMode", exec_mode.clone())],
            ))
        })),
    );
    interpreter.host_modules.insert(
        "snakemake_interface_executor_plugins".into(),
        factory(Rc::new(|interpreter| {
            let settings =
                interpreter.import_module("snakemake_interface_executor_plugins.settings")?;
            Ok(module(
                "snakemake_interface_executor_plugins",
                vec![("settings", settings)],
            ))
        })),
    );
    // peppy, which Snakemake uses for PEP projects and which code may use itself.
    interpreter.host_modules.insert(
        "peppy".into(),
        factory(Rc::new(|_| {
            Ok(module(
                "peppy",
                vec![(
                    "Project",
                    Value::builtin("Project", |interpreter, mut args| {
                        let config = args.take(0, "cfg").unwrap_or(Value::None);
                        let path = python_evaluator::modules::path_text(interpreter, &config)?;
                        crate::pep::load(interpreter, &path)
                    }),
                )],
            ))
        })),
    );
    interpreter.host_modules.insert(
        "snakemake.dag".into(),
        factory(Rc::new(|_| {
            Ok(module(
                "snakemake.dag",
                vec![("toposort", Value::builtin("toposort", toposort))],
            ))
        })),
    );
    interpreter.host_modules.insert(
        "snakemake.common".into(),
        factory(Rc::new(|_| {
            Ok(module(
                "snakemake.common",
                vec![("__version__", Value::str(VERSION))],
            ))
        })),
    );
    interpreter.host_modules.insert(
        "snakemake".into(),
        factory(Rc::new(|interpreter| {
            let mut entries = vec![("__version__", Value::str(VERSION))];
            for name in [
                "utils",
                "io",
                "exceptions",
                "logging",
                "shell",
                "workflow",
                "common",
            ] {
                let submodule = interpreter.import_module(&format!("snakemake.{name}"))?;
                entries.push((name, submodule));
            }
            Ok(module("snakemake", entries))
        })),
    );
}
