//! The host of the build phase: factory and operator calls in workflow bodies become nodes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::glob::{FileType, GlobOptions, glob, is_glob, no_match};
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host};
use nextflow_evaluator::lowered::{Expression, ExpressionKind, Input, Module, Process};
use nextflow_evaluator::value::{ChannelId, ChannelOut, ListKind, PathValue, Value};

use crate::files::CollectFile;
use crate::keyed::{Combine, Cross, GroupTuple, Join};
use crate::network::Network;
use crate::operator::{Each, Factory, Gather, Gathering, Mix, Single};
use crate::process::{ProcessNode, singleton};
use crate::route::{Concat, Merge, Route};

pub(crate) struct Builder<'a> {
    pub network: &'a mut Network,
    pub base: &'a mut dyn Host,
    /// Whether tasks run their `stub:` section.
    pub stub: bool,
    pub config: &'a Config,
}

impl Host for Builder<'_> {
    fn print(&mut self, text: &str) {
        self.base.print(text);
    }

    fn launch_dir(&self) -> String {
        self.base.launch_dir()
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        self.base.download(path)
    }

    fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, Error> {
        self.base.read(path)
    }

    fn metadata(&mut self, path: &PathValue) -> Result<Option<FileMetadata>, Error> {
        self.base.metadata(path)
    }

    fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
        self.base.write(path, bytes)
    }

    fn delete(&mut self, path: &PathValue) -> Result<bool, Error> {
        self.base.delete(path)
    }

    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        self.base.create_directories(path)
    }

    fn environment(&self, name: &str) -> Option<String> {
        self.base.environment(name)
    }

    fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
        self.base.list(directory)
    }

    fn plugin_function(
        &mut self,
        plugin: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.base.plugin_function(plugin, name, arguments)
    }

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        if namespace == "channel" && name != "fromList" && arguments.iter().any(is_channel) {
            let message = format!("Argument of `channel.{name}` cannot be a channel object");
            return Err(thrown(message));
        }
        let (values, value, close) = match (namespace, name, arguments.as_slice()) {
            ("channel", "topic", [topic]) => {
                return Ok(Some(Value::Channel(self.network.topic(&topic.to_string()))));
            }
            ("channel", "fromPath", [.., _]) => (self.path_values(&arguments)?, false, true),
            ("channel", "from", [single @ (Value::List(_) | Value::Set(_))]) => {
                (elements(single), false, true)
            }
            ("channel", "from", _) => (arguments, false, true),
            ("channel", "of", _) => (expand_ranges(arguments), false, true),
            ("channel", "fromList", [list]) => (elements(list), false, true),
            ("channel", "empty", []) => (Vec::new(), false, true),
            // `channel.value()` and `channel.value(null)` are never bound.
            ("channel", "value", [] | [Value::Null]) => (Vec::new(), true, false),
            ("channel", "value", [value]) => (vec![value.clone()], true, true),
            ("log", "info" | "warn" | "error" | "debug", [message]) => {
                let prefix = match name {
                    "warn" => "WARN: ",
                    "error" => "ERROR ~ ",
                    _ => "",
                };
                if name != "debug" {
                    self.base.print(&format!("{prefix}{message}\n"));
                }
                return Ok(Some(Value::Null));
            }
            _ => return Ok(None),
        };
        let output = self.network.channel(value);
        let factory = Factory {
            values,
            output,
            close,
        };
        self.network.add(name, &[], Box::new(factory));
        Ok(Some(Value::Channel(output)))
    }

    /// A process call: each input reads its channel; a plain value becomes a bound value
    /// channel. A process without queue inputs runs one task and emits value channels.
    fn call_process(
        &mut self,
        module: &Rc<Module>,
        process: &Rc<Process>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let arguments: Vec<Value> = arguments
            .into_iter()
            .flat_map(|argument| match argument {
                Value::ChannelOut(out) => out.values.clone(),
                other => vec![other],
            })
            .collect();
        if arguments.len() != process.inputs.len() {
            let message = format!(
                "Process `{name}` declares {} input channels but {} were given",
                process.inputs.len(),
                arguments.len()
            );
            return Err(thrown(message));
        }
        if process
            .inputs
            .iter()
            .any(|input| matches!(input, Input::Each(_)))
        {
            return Err(Error::Unsupported {
                message: "`each` process inputs".into(),
                span: process.span,
            });
        }
        let mut inputs = Vec::new();
        let mut queue_inputs = false;
        for argument in arguments {
            let channel = match argument {
                Value::Channel(channel) => channel,
                Value::Null => {
                    return Err(thrown("A process input channel evaluates to null".into()));
                }
                plain => {
                    let channel = self.network.channel(true);
                    self.network.bind(channel, plain);
                    channel
                }
            };
            queue_inputs |= !self.network.is_value(channel);
            inputs.push(channel);
        }
        // Selectors match the definition name, the alias and the fully qualified name.
        let alias = name.rsplit(':').next().unwrap_or(name);
        let settings = self
            .config
            .process_settings(&[&process.name, alias, name], &labels(process))
            .map_err(|exception| Error::Thrown {
                exception: Rc::new(exception),
                span: process.span,
            })?;
        let singleton = singleton(process, queue_inputs);
        let outputs: Vec<ChannelId> = process
            .outputs
            .iter()
            .map(|_| self.network.channel(singleton))
            .collect();
        let node = ProcessNode {
            module: module.clone(),
            process: process.clone(),
            name: name.into(),
            outputs: outputs.clone(),
            singleton,
            stub: self.stub,
            settings: Rc::new(settings),
            rounds: 0,
            poisoned: false,
            pending: Default::default(),
        };
        self.network.add(name, &inputs, Box::new(node));
        for (output, channel) in process.outputs.iter().zip(&outputs) {
            if let Some(topic) = &output.topic {
                self.network.add_topic_source(topic, *channel);
            }
        }
        let values = outputs.into_iter().map(Value::Channel).collect();
        let names = process
            .outputs
            .iter()
            .map(|output| output.emit.clone())
            .collect();
        Ok(Value::ChannelOut(Rc::new(ChannelOut { values, names })))
    }

    fn operator(
        &mut self,
        source: &Value,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        let (source, arguments) = match source {
            Value::Channel(channel) => (*channel, arguments),
            Value::ChannelOut(out) => match (out.values.as_slice(), arguments.is_empty()) {
                ([], _) => {
                    return Err(thrown(format!(
                        "Operator `{name}` cannot be applied to an undefined output"
                    )));
                }
                ([Value::Channel(channel)], _) => (*channel, arguments),
                ([Value::Channel(first), rest @ ..], true) => (*first, rest.to_vec()),
                _ => {
                    let message = format!(
                        "Multi-channel output cannot be applied to operator {name} for which argument is already provided"
                    );
                    return Err(thrown(message));
                }
            },
            _ => return Ok(None),
        };
        let same_kind = self.network.is_value(source);
        let (kind, value) = match (name, arguments.as_slice()) {
            ("map", [Value::Closure(closure)]) => {
                (Kind::Each(Each::Map(closure.clone())), same_kind)
            }
            ("filter", [criteria]) => (Kind::Each(Each::Filter(criteria.clone())), same_kind),
            ("flatMap", []) => (Kind::Each(Each::FlatMap(None)), false),
            ("flatMap", [Value::Closure(closure)]) => {
                (Kind::Each(Each::FlatMap(Some(closure.clone()))), false)
            }
            ("flatten", []) => (Kind::Each(Each::Flatten), false),
            ("view", rest) => {
                let (options, closure) = options_and_closure(rest);
                let new_line = option(&options, "newLine").is_none_or(|value| value.truth());
                (Kind::Each(Each::View { closure, new_line }), same_kind)
            }
            ("collect", rest) => {
                let (options, closure) = options_and_closure(rest);
                let flat = option(&options, "flat").is_none_or(|value| value.truth());
                (Kind::Gather(Gather::Collect { closure, flat }), true)
            }
            ("toList", []) => (Kind::Gather(Gather::ToList), true),
            ("ifEmpty", [default]) => (Kind::Gather(Gather::IfEmpty(default.clone())), same_kind),
            ("first", []) => (Kind::Gather(Gather::First(None)), true),
            ("first", [criteria]) => (Kind::Gather(Gather::First(Some(criteria.clone()))), true),
            ("mix", others) if !others.is_empty() => {
                let mut inputs = vec![source];
                for other in others {
                    inputs.extend(
                        channels(other)
                            .ok_or_else(|| thrown(format!("Invalid `mix` operand: {other}")))?,
                    );
                }
                let output = self.network.channel(false);
                let closed = vec![false; inputs.len()];
                self.network
                    .add(name, &inputs, Box::new(Mix { output, closed }));
                return Ok(Some(Value::Channel(output)));
            }
            ("mix", []) => {
                return Err(thrown(
                    "Operator 'mix' should have at least one right operand".into(),
                ));
            }
            ("unique", rest) => {
                let (_, closure) = options_and_closure(rest);
                let seen = Vec::new();
                (Kind::Each(Each::Unique { closure, seen }), same_kind)
            }
            ("distinct", rest) => {
                let (_, closure) = options_and_closure(rest);
                let previous = Value::Null;
                (Kind::Each(Each::Distinct { closure, previous }), same_kind)
            }
            ("take", [Value::Number(count)]) => {
                let count = count.to_i64();
                (
                    Kind::Each(Each::Take(if count < 0 { -1 } else { count })),
                    false,
                )
            }
            ("until", [Value::Closure(closure)]) => {
                (Kind::Each(Each::Until(closure.clone())), same_kind)
            }
            ("transpose", rest) => {
                let (options, _) = options_and_closure(rest);
                let by = indexes(&options, "by");
                let remainder = option(&options, "remainder").is_some_and(|value| value.truth());
                (Kind::Each(Each::Transpose { by, remainder }), false)
            }
            // `dump` prints only with `-dump-channels`, which iunctus does not have, so it is
            // the source itself.
            ("dump", _) => return Ok(Some(Value::Channel(source))),
            ("toSortedList", rest) => {
                let (_, closure) = options_and_closure(rest);
                (Kind::Gather(Gather::ToSortedList(closure)), true)
            }
            ("count", []) => (Kind::Gather(Gather::Count(None)), true),
            ("count", [criteria]) => (Kind::Gather(Gather::Count(Some(criteria.clone()))), true),
            ("sum" | "min" | "max", rest) => {
                let (_, closure) = options_and_closure(rest);
                let kind = match name {
                    "sum" => Gather::Sum(closure),
                    "min" => Gather::Min(closure),
                    _ => Gather::Max(closure),
                };
                (Kind::Gather(kind), true)
            }
            ("reduce", [Value::Closure(closure)]) => {
                (Kind::Gather(Gather::Reduce(closure.clone())), true)
            }
            ("reduce", [seed, Value::Closure(closure)]) => {
                let output = self.network.channel(true);
                let gathering = Gathering {
                    kind: Gather::Reduce(closure.clone()),
                    output,
                    items: vec![seed.clone()],
                    done: false,
                };
                self.network.add(name, &[source], Box::new(gathering));
                return Ok(Some(Value::Channel(output)));
            }
            ("last", []) => (Kind::Gather(Gather::Last), true),
            _ => return self.keyed_operator(source, name, &arguments),
        };
        let output = self.network.channel(value);
        match kind {
            Kind::Each(kind) => {
                self.network
                    .add(name, &[source], Box::new(Single { kind, output }))
            }
            Kind::Gather(kind) => {
                let gathering = Gathering {
                    kind,
                    output,
                    items: Vec::new(),
                    done: false,
                };
                self.network.add(name, &[source], Box::new(gathering));
            }
        }
        Ok(Some(Value::Channel(output)))
    }
}

impl Builder<'_> {
    /// The paths of `channel.fromPath(pattern, [options])`: each pattern in turn, a glob
    /// expanded to the matching files sorted by path.
    fn path_values(&mut self, arguments: &[Value]) -> Result<Vec<Value>, Error> {
        let (options, patterns) = match arguments {
            [options @ Value::Map(_), patterns] => (Some(options.clone()), patterns),
            [patterns] => (None, patterns),
            _ => return Err(thrown("Missing `fromPath` parameter".into())),
        };
        let file_type = match option(&options, "type")
            .map(|kind| kind.to_string())
            .as_deref()
        {
            None | Some("file") => FileType::File,
            Some("dir") => FileType::Directory,
            Some("any") => FileType::Any,
            Some(other) => return Err(thrown(format!("Not a valid type: {other}"))),
        };
        let glob_options = GlobOptions {
            file_type,
            hidden: option(&options, "hidden").map(|value| value.truth()),
            max_depth: option(&options, "maxDepth").and_then(|depth| match depth {
                Value::Number(depth) => Some(depth.to_i64() as usize),
                _ => None,
            }),
        };
        let check = option(&options, "checkIfExists").is_some_and(|value| value.truth());
        let globbing = option(&options, "glob").is_none_or(|value| value.truth());
        let mut paths = Vec::new();
        for pattern in elements(patterns) {
            let text = match &pattern {
                Value::Path(path) => path.uri(),
                other => other.to_string(),
            };
            if globbing && is_glob(&text) {
                let found = glob(&mut *self.base, &text, &glob_options)?;
                if found.is_empty() && check {
                    return Err(no_match(&text));
                }
                paths.extend(found);
                continue;
            }
            let path = PathValue::parse(&text, &self.base.launch_dir());
            if check && self.base.metadata(&path)?.is_none() {
                let message = format!("No such file or directory: {}", path.uri());
                return Err(Error::Thrown {
                    exception: Rc::new(Exception::new("NoSuchFileException", message)),
                    span: Default::default(),
                });
            }
            paths.push(path);
        }
        Ok(paths
            .into_iter()
            .map(|path| Value::Path(Rc::new(path)))
            .collect())
    }
}

/// The labels of a process, from its `label` directives, which must be plain strings.
fn labels(process: &Process) -> Vec<String> {
    process
        .directives
        .iter()
        .filter(|directive| &*directive.name == "label")
        .filter_map(|directive| match directive.arguments.positional.first() {
            Some(Expression {
                kind: ExpressionKind::Literal(label),
                ..
            }) => Some(label.to_string()),
            _ => None,
        })
        .collect()
}

impl Builder<'_> {
    /// The operators with several inputs or outputs: `groupTuple`, `join`, `combine`,
    /// `cross`, `concat`, `merge`, `branch` and `multiMap`.
    fn keyed_operator(
        &mut self,
        source: ChannelId,
        name: &str,
        arguments: &[Value],
    ) -> Result<Option<Value>, Error> {
        let (options, rest): (Option<Value>, &[Value]) = match arguments {
            [options @ Value::Map(_), rest @ ..] => (Some(options.clone()), rest),
            _ => (None, arguments),
        };
        let queue = |builder: &mut Self| builder.network.channel(false);
        let node: (Vec<ChannelId>, Box<dyn crate::network::Behaviour>, Value) = match (name, rest) {
            ("groupTuple", []) => {
                let output = queue(self);
                let size = option(&options, "size").and_then(|size| match size {
                    Value::Number(size) if size.to_i64() > 0 => Some(size.to_i64() as usize),
                    _ => None,
                });
                let node = GroupTuple {
                    output,
                    by: indexes(&options, "by").unwrap_or(vec![0]),
                    size,
                    remainder: option(&options, "remainder").is_some_and(|value| value.truth()),
                    sort: option(&options, "sort").is_some_and(|value| value.truth()),
                    groups: Default::default(),
                };
                (vec![source], Box::new(node), Value::Channel(output))
            }
            ("join", [right]) => {
                let Some([right]) = channels(right)
                    .as_deref()
                    .map(<[ChannelId; 1]>::try_from)
                    .and_then(Result::ok)
                else {
                    return Err(thrown(format!("Invalid `join` operand: {right}")));
                };
                let output = queue(self);
                let node = Join {
                    output,
                    by: indexes(&options, "by").unwrap_or(vec![0]),
                    remainder: option(&options, "remainder").is_some_and(|value| value.truth()),
                    pending: Default::default(),
                    done: [false; 2],
                    has_values: false,
                };
                (vec![source, right], Box::new(node), Value::Channel(output))
            }
            ("combine", [right]) => {
                let output = queue(self);
                let by = indexes(&options, "by");
                let (inputs, seen, done) = match channels(right).as_deref() {
                    Some([right]) => (vec![source, *right], Vec::new(), false),
                    _ => (vec![source], elements(right), true),
                };
                let node = Combine {
                    output,
                    by,
                    seen: [Vec::new(), seen],
                    done: [false, done],
                };
                (inputs, Box::new(node), Value::Channel(output))
            }
            ("cross", [right, ..]) => {
                let Some([right]) = channels(right)
                    .as_deref()
                    .map(<[ChannelId; 1]>::try_from)
                    .and_then(Result::ok)
                else {
                    return Err(thrown(format!("Invalid `cross` operand: {right}")));
                };
                let output = queue(self);
                let key = match rest {
                    [_, Value::Closure(closure)] => Some(closure.clone()),
                    _ => None,
                };
                let node = Cross {
                    output,
                    key,
                    left: Default::default(),
                    waiting: Default::default(),
                    done: [false; 2],
                };
                (vec![source, right], Box::new(node), Value::Channel(output))
            }
            ("concat", others) => {
                let mut inputs = vec![source];
                for other in others {
                    inputs.extend(
                        channels(other)
                            .ok_or_else(|| thrown(format!("Invalid `concat` operand: {other}")))?,
                    );
                }
                let output = queue(self);
                (
                    inputs,
                    Box::new(Concat { output, current: 0 }),
                    Value::Channel(output),
                )
            }
            ("merge", others) => {
                let mut inputs = vec![source];
                let mut closure = None;
                for other in others {
                    match other {
                        Value::Closure(value) => closure = Some(value.clone()),
                        other => {
                            inputs.extend(channels(other).ok_or_else(|| {
                                thrown(format!("Invalid `merge` operand: {other}"))
                            })?)
                        }
                    }
                }
                let output = self.network.channel(self.network.is_value(source));
                (
                    inputs,
                    Box::new(Merge { output, closure }),
                    Value::Channel(output),
                )
            }
            ("collectFile", rest) => {
                let output = queue(self);
                let closure = match rest {
                    [Value::Closure(closure)] => Some(closure.clone()),
                    _ => None,
                };
                let launch = self.base.launch_dir();
                let mut name = option(&options, "name").map(|name| match name {
                    Value::Path(path) => path.uri(),
                    other => other.to_string(),
                });
                let mut directory = match option(&options, "storeDir") {
                    Some(Value::Path(path)) => (*path).clone(),
                    Some(other) => PathValue::parse(&other.to_string(), &launch),
                    None => PathValue::parse(&format!("work/collect-file/{}", output.0), &launch),
                };
                // A name with a directory also chooses where the file goes.
                if let Some(text) = name.clone().filter(|text| text.contains('/')) {
                    let path = PathValue::parse(&text, &launch);
                    if let Some(parent) = path.parent() {
                        directory = parent;
                    }
                    name = Some(path.name().to_owned());
                }
                let sort = !matches!(
                    option(&options, "sort")
                        .map(|sort| sort.to_string())
                        .as_deref(),
                    Some("false" | "none" | "index")
                );
                let keep_header = option(&options, "keepHeader").is_some_and(|value| value.truth());
                let skip = match option(&options, "skip") {
                    Some(Value::Number(skip)) => skip.to_i64() as usize,
                    _ => usize::from(keep_header),
                };
                let node = CollectFile {
                    output,
                    closure,
                    name,
                    directory,
                    new_line: option(&options, "newLine").is_some_and(|value| value.truth()),
                    sort,
                    skip,
                    keep_header,
                    seed: option(&options, "seed").map(|seed| seed.to_string()),
                    files: Default::default(),
                };
                (vec![source], Box::new(node), Value::Channel(output))
            }
            ("branch" | "multiMap", criteria) => {
                let (labels, closure) = match criteria {
                    [Value::List(labels), Value::Closure(closure)] => {
                        (labels.clone(), closure.clone())
                    }
                    [Value::List(pair)] => match pair.borrow().items.as_slice() {
                        [Value::List(labels), Value::Closure(closure)] => {
                            (labels.clone(), closure.clone())
                        }
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                };
                let labels = labels.borrow().items.clone();
                let value = self.network.is_value(source);
                let outputs: Vec<ChannelId> =
                    labels.iter().map(|_| self.network.channel(value)).collect();
                let node = Route {
                    criteria: closure,
                    labels: labels.clone(),
                    outputs: outputs.clone(),
                    all: name == "multiMap",
                };
                let names = labels
                    .iter()
                    .map(|label| Some(label.to_string().into()))
                    .collect();
                let values = outputs.into_iter().map(Value::Channel).collect();
                let out = Value::ChannelOut(Rc::new(ChannelOut { values, names }));
                (vec![source], Box::new(node), out)
            }
            _ => return Ok(None),
        };
        let (inputs, behaviour, result) = node;
        self.network.add(name, &inputs, behaviour);
        Ok(Some(result))
    }
}

/// An option that holds an index or a list of indexes, like `by: [0, 1]`.
fn indexes(options: &Option<Value>, name: &str) -> Option<Vec<usize>> {
    match option(options, name)? {
        Value::Number(index) => Some(vec![index.to_i64() as usize]),
        Value::List(list) => Some(
            list.borrow()
                .items
                .iter()
                .filter_map(|item| match item {
                    Value::Number(index) => Some(index.to_i64() as usize),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

enum Kind {
    Each(Each),
    Gather(Gather),
}

/// The channels of a channel or of the outputs of a call.
fn channels(value: &Value) -> Option<Vec<ChannelId>> {
    match value {
        Value::Channel(channel) => Some(vec![*channel]),
        Value::ChannelOut(out) => out
            .values
            .iter()
            .map(|value| match value {
                Value::Channel(channel) => Some(*channel),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn is_channel(value: &Value) -> bool {
    matches!(value, Value::Channel(_) | Value::ChannelOut(_))
}

/// The arguments of `channel.of`: a range gives its elements, a list stays one value.
fn expand_ranges(arguments: Vec<Value>) -> Vec<Value> {
    let mut values = Vec::new();
    for argument in arguments {
        match &argument {
            Value::List(list) if list.borrow().kind == ListKind::Range => {
                values.extend(list.borrow().items.clone());
            }
            _ => values.push(argument),
        }
    }
    values
}

fn elements(value: &Value) -> Vec<Value> {
    match value {
        Value::List(list) => list.borrow().items.clone(),
        Value::Set(set) => set
            .borrow()
            .items
            .iter()
            .map(|item| item.0.clone())
            .collect(),
        other => vec![other.clone()],
    }
}

/// Named arguments, which come first as a map, and a trailing closure.
fn options_and_closure(
    arguments: &[Value],
) -> (
    Option<Value>,
    Option<Rc<nextflow_evaluator::value::Closure>>,
) {
    let mut options = None;
    let mut closure = None;
    for argument in arguments {
        match argument {
            Value::Map(_) => options = Some(argument.clone()),
            Value::Closure(value) => closure = Some(value.clone()),
            _ => {}
        }
    }
    (options, closure)
}

fn option(options: &Option<Value>, name: &str) -> Option<Value> {
    match options {
        Some(Value::Map(map)) => map
            .borrow()
            .entries
            .get(&nextflow_evaluator::value::Key(Value::string(name)))
            .cloned(),
        _ => None,
    }
}

fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}
