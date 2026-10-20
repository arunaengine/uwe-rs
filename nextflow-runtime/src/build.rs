//! The host of the build phase: factory and operator calls in workflow bodies become nodes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::glob::{FileType, GlobOptions, glob, is_glob, no_match};
use nextflow_evaluator::interpreter::{Error, FileMetadata, Host, OutputOptions};
use nextflow_evaluator::lowered::{Expression, ExpressionKind, Input, Module, Process};
use nextflow_evaluator::split::{Splitter, positive};
use nextflow_evaluator::value::{ChannelId, ChannelOut, ListKind, PathValue, Value};

use crate::files::CollectFile;
use crate::keyed::{Combine, Cross, GroupTuple, Join};
use crate::network::Network;
use crate::operator::{
    Buffer, Closing, Collate, Each, Factory, Gather, Gathering, Invoke, Mix, Opening, Single, Sort,
};
use crate::process::{ProcessNode, singleton};
use crate::route::{Concat, Merge, Route};
use crate::split::{Split, SplitFiles, Subscribe};

/// The channel operators of Nextflow (`OperatorImpl` and `ChannelEx`). Another method of a
/// value channel is a method of its value.
const OPERATORS: &[&str] = &[
    "branch",
    "buffer",
    "chain",
    "collate",
    "collect",
    "collectFile",
    "combine",
    "concat",
    "count",
    "countFasta",
    "countFastq",
    "countJson",
    "countLines",
    "cross",
    "distinct",
    "dump",
    "filter",
    "first",
    "flatMap",
    "flatten",
    "groupBy",
    "groupTuple",
    "ifEmpty",
    "join",
    "last",
    "map",
    "max",
    "mean",
    "merge",
    "min",
    "mix",
    "multiMap",
    "randomSample",
    "reduce",
    "set",
    "splitCsv",
    "splitFasta",
    "splitFastq",
    "splitJson",
    "splitText",
    "subscribe",
    "sum",
    "take",
    "tap",
    "toDouble",
    "toFloat",
    "toInteger",
    "toList",
    "toLong",
    "toSortedList",
    "transpose",
    "unique",
    "until",
    "view",
];

pub(crate) struct Builder<'a> {
    pub network: &'a mut Network,
    pub base: &'a mut dyn Host,
    /// Whether tasks run their `stub:` section.
    pub stub: bool,
    pub config: &'a Config,
    /// The output directory of the workflow outputs from `-output-dir`, over `outputDir`.
    pub output_dir: Option<String>,
    /// Where `exec:` sections run, if they may (`Options::exec_folder`).
    pub exec_folder: Option<Rc<std::path::Path>>,
}

impl Host for Builder<'_> {
    fn print(&mut self, text: &str) {
        self.base.print(text);
    }

    fn simulated(&self) -> bool {
        self.base.simulated()
    }

    fn launch_dir(&self) -> String {
        self.base.launch_dir()
    }

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        self.base.operator_directory()
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

    fn spread(&mut self, path: &PathValue) {
        self.base.spread(path);
    }

    fn validate_json(&mut self, schema: &str, instance: &str) -> Result<Option<String>, Error> {
        self.base.validate_json(schema, instance)
    }

    fn bound_channel(&mut self, value: Value) -> Option<Value> {
        let output = self.network.channel(true);
        self.network.bind(output, value);
        Some(Value::Channel(output))
    }

    /// A property of the value of a value channel is a value channel of that property, as a
    /// GPars `DataflowVariable` gives it.
    fn channel_property(&mut self, channel: &Value, name: &str) -> Result<Option<Value>, Error> {
        let Value::Channel(source) = channel else {
            return Ok(None);
        };
        if !self.network.is_value(*source) {
            return Ok(None);
        }
        // `getVal()` of a value channel that is bound already gives its value, as Nextflow's
        // `DataflowVariable` does while the workflow is built.
        if matches!(name, "val" | "value")
            && let Some(value) = self.network.bound(*source)
        {
            return Ok(Some(value));
        }
        let output = self.network.channel(true);
        let node = Invoke {
            output,
            name: name.into(),
            arguments: Vec::new(),
            property: true,
        };
        self.network.add(name, &[*source], Box::new(node));
        Ok(Some(Value::Channel(output)))
    }

    fn create_directories(&mut self, path: &PathValue) -> Result<(), Error> {
        self.base.create_directories(path)
    }

    fn secret(&mut self, name: &str) -> Result<Option<String>, Error> {
        self.base.secret(name)
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

    /// A workflow output publishes the files of the values of its channel, with the options
    /// of the `output { }` block over those of the config's `workflow.output` scope.
    fn publish(&mut self, name: &str, channel: Value, options: OutputOptions) -> Result<(), Error> {
        let Value::Channel(channel) = channel else {
            return Err(thrown(format!("Invalid workflow output `{name}`")));
        };
        let setting = |option: &str| {
            (options.get(option).cloned())
                .or_else(|| self.config.get(&["workflow", "output", option]))
        };
        let label = format!("output {name}");
        if setting("enabled").is_some_and(|enabled| !enabled.truth()) {
            let node = Subscribe {
                on_error: None,
                on_next: None,
                on_complete: None,
            };
            self.network.add(&label, &[channel], Box::new(node));
            return Ok(());
        }
        let mode = match setting("mode") {
            Some(mode) => crate::publish::mode_of(&mode.to_string())?,
            None => workflow_graph::DeliveryMode::Link,
        };
        let overwrite = setting("overwrite").is_none_or(|overwrite| overwrite.truth());
        let configured = (self.output_dir.clone()).or_else(|| {
            self.config
                .get(&["outputDir"])
                .map(|folder| folder.to_string())
        });
        let launch = std::path::PathBuf::from(self.base.launch_dir());
        let node = crate::outputs::Publish {
            name: name.into(),
            options,
            directory: crate::outputs::output_directory(configured.as_deref(), &launch),
            mode,
            overwrite,
            published: Vec::new(),
        };
        self.network.add(&label, &[channel], Box::new(node));
        Ok(())
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
        // A factory of a plugin, like `Channel.fromSamplesheet`, gives the list of its values.
        if namespace == "channel"
            && let Some(plugin) = nextflow_plugins::factory_plugin(name)
        {
            let values = self.base.plugin_function(plugin, name, arguments)?;
            return Ok(Some(self.factory(name, elements(&values), false, true)));
        }
        let (values, value, close) = match (namespace, name, arguments.as_slice()) {
            ("channel", "topic", [topic]) => {
                return Ok(Some(Value::Channel(self.network.topic(&topic.to_string()))));
            }
            // A pattern that is null or empty fails at once, as Nextflow checks it first.
            ("channel", "fromPath", [.., pattern]) if !pattern.truth() => {
                let message = "Missing `fromPath` parameter";
                return Err(Error::Thrown {
                    exception: Rc::new(Exception::new("AbortOperationException", message)),
                    span: Default::default(),
                });
            }
            // A missing file fails the run when the factory starts, as in Nextflow, so that
            // errors of the workflow body come first.
            ("channel", "fromPath", [.., _]) => match self.path_values(&arguments) {
                Ok(values) => (values, false, true),
                Err(failure @ Error::Thrown { .. }) => {
                    return Ok(Some(self.failing_factory(name, failure)));
                }
                Err(error) => return Err(error),
            },
            ("channel", "fromFilePairs", [.., _]) => match self.file_pair_values(&arguments) {
                Ok(values) => (values, false, true),
                Err(failure @ Error::Thrown { .. }) => {
                    return Ok(Some(self.failing_factory(name, failure)));
                }
                Err(error) => return Err(error),
            },
            ("channel", "from", [single @ (Value::List(_) | Value::Set(_))]) => {
                (elements(single), false, true)
            }
            ("channel", "from", _) => (arguments, false, true),
            // A Java array as the only argument is the list of values, as Groovy passes it.
            ("channel", "of", [Value::List(list)]) if list.borrow().kind == ListKind::Array => {
                (list.borrow().items.clone(), false, true)
            }
            ("channel", "of", _) => (expand_ranges(arguments), false, true),
            ("channel", "fromList", [list]) => (elements(list), false, true),
            ("channel", "empty", []) => (Vec::new(), false, true),
            // `channel.value()` and `channel.value(null)` are never bound.
            ("channel", "value", [] | [Value::Null]) => (Vec::new(), true, false),
            ("channel", "value", [value]) => (vec![value.clone()], true, true),
            _ => return self.base.namespace_method(namespace, name, arguments),
        };
        Ok(Some(self.factory(name, values, value, close)))
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
        let mut inputs = Vec::new();
        let mut queue_inputs = false;
        for (argument, input) in arguments.into_iter().zip(&process.inputs) {
            let argument = match (input, argument) {
                // An `each` input takes all values of a queue channel as one list, as
                // Nextflow's `EachInParam` does.
                (Input::Each(_), Value::Channel(channel)) if !self.network.is_value(channel) => {
                    let source = Value::Channel(channel);
                    self.operator(&source, "toList", Vec::new())?
                        .unwrap_or(source)
                }
                (_, argument) => argument,
            };
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
            conda_enabled: self
                .config
                .get(&["conda", "enabled"])
                .is_some_and(|v| v.truth()),
            spack_enabled: self
                .config
                .get(&["spack", "enabled"])
                .is_some_and(|v| v.truth()),
            module: module.clone(),
            process: process.clone(),
            name: name.into(),
            outputs: outputs.clone(),
            singleton,
            one_round: !queue_inputs,
            stub: self.stub,
            exec_folder: self.exec_folder.clone(),
            settings: Rc::new(settings),
            rounds: 0,
            tasks: 0,
            max_forks: None,
            fair: false,
            held: Default::default(),
            next_emission: 1,
            poisoned: false,
            pending: Default::default(),
            each_round: None,
        };
        self.network.add(name, &inputs, Box::new(node));
        for (output, channel) in process.outputs.iter().zip(&outputs) {
            if let Some(topic) = &output.topic {
                self.network.add_topic_source(topic, *channel);
            }
        }
        // The entries of a `topic:` section only feed their topics.
        let shown = || {
            process
                .outputs
                .iter()
                .zip(&outputs)
                .filter(|(output, _)| !output.hidden)
        };
        let values = shown()
            .map(|(_, channel)| Value::Channel(*channel))
            .collect();
        let names = shown().map(|(output, _)| output.emit.clone()).collect();
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
        // The text of a GPars channel, as Nextflow 26.04.6 prints it.
        if name == "toString" && arguments.is_empty() {
            let text = match same_kind {
                true => {
                    let value = self.network.bound(source).unwrap_or(Value::Null);
                    format!("DataflowVariable(value={value})")
                }
                false => "DataflowBroadcast around DataflowStream[?]".into(),
            };
            return Ok(Some(Value::string(text)));
        }
        let own = matches!(name, "getClass" | "toString" | "hashCode" | "equals");
        if same_kind && !own && !OPERATORS.contains(&name) {
            let output = self.network.channel(true);
            let mut inputs = vec![source];
            let arguments = arguments
                .into_iter()
                .map(|argument| match argument {
                    Value::Channel(channel) if self.network.is_value(channel) => {
                        inputs.push(channel);
                        None
                    }
                    other => Some(other),
                })
                .collect();
            let node = Invoke {
                output,
                name: name.into(),
                arguments,
                property: false,
            };
            self.network.add(name, &inputs, Box::new(node));
            return Ok(Some(Value::Channel(output)));
        }
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
            ("toInteger" | "toLong" | "toFloat" | "toDouble", []) => {
                let class = match name {
                    "toInteger" => "Integer",
                    "toLong" => "Long",
                    "toFloat" => "Float",
                    _ => "Double",
                };
                (Kind::Each(Each::Cast(class)), same_kind)
            }
            ("view", rest) => {
                let (options, closure) = options_and_closure(rest);
                let new_line = option(&options, "newLine").is_none_or(|value| value.truth());
                (Kind::Each(Each::View { closure, new_line }), same_kind)
            }
            ("collect", rest) => {
                let (options, closure) = options_and_closure(rest);
                let flat = option(&options, "flat").is_none_or(|value| value.truth());
                // `sort: true` sorts in the natural order; a closure is a key or a comparator.
                let (sort, sort_by) = match option(&options, "sort") {
                    Some(Value::Closure(by)) => (true, Some(by)),
                    Some(sort) => (sort.truth(), None),
                    None => (false, None),
                };
                let collect = Gather::Collect {
                    closure,
                    flat,
                    sort,
                    sort_by,
                };
                (Kind::Gather(collect), true)
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
            ("collate", [Value::Number(size), rest @ ..]) if size.to_i64() > 0 => {
                let size = size.to_i64() as usize;
                let (step, remainder) = match rest {
                    [] => (size, true),
                    [Value::Boolean(remainder)] => (size, *remainder),
                    [Value::Number(step)] => (step.to_i64().max(1) as usize, true),
                    [Value::Number(step), Value::Boolean(remainder)] => {
                        (step.to_i64().max(1) as usize, *remainder)
                    }
                    _ => return Ok(None),
                };
                let output = self.network.channel(false);
                let node = Collate {
                    output,
                    size,
                    step,
                    remainder,
                    windows: Vec::new(),
                    count: 0,
                };
                self.network.add(name, &[source], Box::new(node));
                return Ok(Some(Value::Channel(output)));
            }
            ("buffer", rest) => {
                let (options, criteria) = match rest {
                    [options @ Value::Map(_)] => (Some(options.clone()), Vec::new()),
                    [options @ Value::Map(_), closing] => {
                        (Some(options.clone()), vec![closing.clone()])
                    }
                    criteria => (None, criteria.to_vec()),
                };
                let count = |name: &str| match option(&options, name) {
                    Some(Value::Number(number)) => number.to_i64().max(0) as usize,
                    _ => 0,
                };
                let (size, skip) = (count("size"), count("skip"));
                let (opening, closing) = match criteria.as_slice() {
                    [] if size > 0 || skip > 0 => (
                        Opening::Skip { skip, count: 0 },
                        Closing::Size {
                            size,
                            skip,
                            count: 0,
                        },
                    ),
                    [closing] => (Opening::Always, Closing::Criteria(closing.clone())),
                    [opening, closing] => (
                        Opening::Criteria(opening.clone()),
                        Closing::Criteria(closing.clone()),
                    ),
                    _ => return Err(thrown("Invalid arguments for operator `buffer`".into())),
                };
                let output = self.network.channel(false);
                let node = Buffer {
                    output,
                    open: matches!(opening, Opening::Always),
                    opening,
                    closing,
                    remainder: option(&options, "remainder").is_some_and(|value| value.truth()),
                    buffer: Vec::new(),
                };
                self.network.add(name, &[source], Box::new(node));
                return Ok(Some(Value::Channel(output)));
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
    /// A channel factory node that emits `values`, into a value channel when `value`, and
    /// closes its channel when `close`.
    fn factory(&mut self, name: &str, values: Vec<Value>, value: bool, close: bool) -> Value {
        let output = self.network.channel(value);
        let factory = Factory {
            values,
            output,
            close,
            failure: None,
        };
        self.network.add(name, &[], Box::new(factory));
        Value::Channel(output)
    }

    /// A channel factory node that fails when it starts.
    fn failing_factory(&mut self, name: &str, failure: Error) -> Value {
        let output = self.network.channel(false);
        let factory = Factory {
            values: Vec::new(),
            output,
            close: true,
            failure: Some(failure),
        };
        self.network.add(name, &[], Box::new(factory));
        Value::Channel(output)
    }

    /// The tuples of `channel.fromFilePairs(pattern, [options])`: the files of each pattern,
    /// found as `fromPath` finds them, grouped by the start of their names.
    fn file_pair_values(&mut self, arguments: &[Value]) -> Result<Vec<Value>, Error> {
        let (options, patterns) = match arguments {
            [options @ Value::Map(_), patterns] => (Some(options.clone()), patterns),
            [patterns] => (None, patterns),
            _ => return Err(thrown("Missing `fromFilePairs` parameter".into())),
        };
        let mut found = Vec::new();
        for pattern in elements(patterns) {
            let text = match &pattern {
                Value::Path(path) => path.uri(),
                other => other.to_string(),
            };
            let mut arguments: Vec<Value> = options.iter().cloned().collect();
            arguments.push(pattern);
            found.push((text, self.path_values(&arguments)?));
        }
        let size = option(&options, "size").and_then(|size| match size {
            Value::Number(size) => Some(size.to_i64()),
            _ => None,
        });
        let flat = matches!(option(&options, "flat"), Some(Value::Boolean(true)));
        crate::pairs::file_pairs(found, size, flat)
    }

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
                    sort: Sort::new(option(&options, "sort"), "groupTuple")?,
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
                let remainder = option(&options, "remainder").is_some_and(|value| value.truth());
                let fail_on_mismatch =
                    option(&options, "failOnMismatch").is_some_and(|value| value.truth());
                if remainder && fail_on_mismatch {
                    return Err(thrown(
                        "`join` cannot combine remainder with failOnMismatch".into(),
                    ));
                }
                let output = queue(self);
                let node = Join {
                    output,
                    by: indexes(&options, "by").unwrap_or(vec![0]),
                    remainder,
                    fail_on_duplicate: option(&options, "failOnDuplicate")
                        .is_some_and(|value| value.truth()),
                    fail_on_mismatch,
                    matched: Default::default(),
                    pending: Default::default(),
                    keys: Default::default(),
                    done: [false; 2],
                    singleton: [None; 2],
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
                let sort = Sort::new(option(&options, "sort"), "collectFile")?;
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
                let explicit_name = name.clone().filter(|text| text.contains('/'));
                let mut directory = match option(&options, "storeDir") {
                    Some(Value::Path(path)) => (*path).clone(),
                    Some(other) => PathValue::parse(&other.to_string(), &launch),
                    None if explicit_name.is_some() => PathValue::parse(&launch, "/"),
                    None => self
                        .base
                        .operator_directory()?
                        .resolve(&format!("collect-file/{}", output.0)),
                };
                // A name with a directory also chooses where the file goes.
                if let Some(text) = explicit_name {
                    let path = PathValue::parse(&text, &launch);
                    if let Some(parent) = path.parent() {
                        directory = parent;
                    }
                    name = Some(path.name().to_owned());
                }
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
            (
                "splitCsv" | "splitText" | "splitFasta" | "splitFastq" | "splitJson" | "countLines"
                | "countFasta" | "countFastq",
                [] | [Value::Closure(_)],
            ) => {
                // The count operators give the number of records in a value channel.
                let counting = name.starts_with("count");
                let output = self.network.channel(counting);
                let mut splitter = Splitter::new(name, options.as_ref());
                splitter.simulated = self.base.simulated();
                let files = match option(&options, "file").filter(|value| value.truth()) {
                    Some(Value::Path(path)) => {
                        let directory = self
                            .base
                            .metadata(&path)?
                            .is_some_and(|metadata| metadata.directory);
                        let (directory, name) = match path.parent() {
                            Some(parent) if !directory => (parent, Some(path.name().to_owned())),
                            _ => ((*path).clone(), None),
                        };
                        Some(SplitFiles::Named(directory, name))
                    }
                    Some(Value::String(name) | Value::GString(name)) => Some(SplitFiles::Named(
                        self.base
                            .operator_directory()?
                            .resolve(&format!("split/{}", output.0)),
                        Some(name.to_string()),
                    )),
                    Some(_) => Some(SplitFiles::Implicit(
                        self.base
                            .operator_directory()?
                            .resolve(&format!("split/{}", output.0)),
                    )),
                    None => None,
                };
                let node = Split {
                    output,
                    splitter,
                    elem: positive(options.as_ref(), "elem"),
                    each: match (rest, option(&options, "each")) {
                        ([Value::Closure(closure)], _) => Some(closure.clone()),
                        ([], Some(Value::Closure(closure))) => Some(closure),
                        _ => None,
                    },
                    files,
                    decompress: option(&options, "decompress").is_none_or(|value| value.truth()),
                    count: 0,
                    total: counting.then_some(0),
                    pending: None,
                };
                (vec![source], Box::new(node), Value::Channel(output))
            }
            ("subscribe", rest) => {
                let on_error = match option(&options, "onError") {
                    Some(Value::Closure(closure)) => Some(closure),
                    None | Some(Value::Null) => None,
                    Some(_) => return Err(thrown("subscribe `onError` must be a closure".into())),
                };
                let closure = |value: Option<Value>| match value {
                    Some(Value::Closure(closure)) => Some(closure),
                    _ => None,
                };
                let node = Subscribe {
                    on_error,
                    on_next: closure(rest.first().cloned().or_else(|| option(&options, "onNext"))),
                    on_complete: closure(option(&options, "onComplete")),
                };
                // Nextflow 26.04 gives nothing, but up to 25.10 `subscribe` gave its source,
                // which pipelines written for those releases read further.
                (vec![source], Box::new(node), Value::Channel(source))
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
