//! Lowers a CWL command line tool into a wir task, when the task builds the same command.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::path::Path;

use cwl_parser::model::{
    Argument as CwlArgument, CommandLineBinding, CommandLineTool, Dynamic, Expression, LoadListing,
    Part, Process, RequirementClass, Type as Cwl, short_name,
};
use wir::{
    Argument, Attributes, Capture, CaptureRule, Content, FileName, FileType, Port, Task, Type,
};

use crate::Scope;
use crate::defaults;
use crate::requirements;
use crate::runtime;
use crate::types::convert;

/// The image of tools without `DockerRequirement`, as uwe runs them.
pub const DEFAULT_IMAGE: &str = "debian:stable-slim";

/// A tool as a task with its ports and the attributes from its requirements.
pub struct Lowered {
    pub task: Task,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub attributes: Attributes,
    /// The task inputs that exprs fill: arguments and the `valueFrom` of input bindings.
    pub values: Vec<Computed>,
    /// The outputs that `outputEval` computes, from the files of the task output of that name.
    pub evaluated: Vec<Evaluated>,
}

/// A task input that an expr fills, with the code, the task input it sees as `self`, and
/// its type, by default words of the command line.
pub struct Computed {
    pub port: String,
    pub code: String,
    pub this: Option<String>,
    pub r#type: Option<Type>,
}

/// An output of a tool that an expr computes from the files that its glob found, if any.
pub struct Evaluated {
    pub output: Port,
    pub code: Expression,
    /// Whether the task has an output of this name with the files.
    pub captured: bool,
}

/// Makes the capture rule of the globs of an output, by its name.
type Globs<'a> = dyn FnMut(&str, &[Expression]) -> Result<CaptureRule, String> + 'a;

/// How the task gives an output: captured as it is, or as files for an `outputEval`.
enum Read {
    Direct(CaptureRule, Type),
    Evaluated(Option<(CaptureRule, Type)>, Expression),
}

/// The task of a tool, or the first thing it uses that a task cannot say the same way. File
/// defaults are relative to `base`.
pub fn lower(
    process: &Process,
    tool: &CommandLineTool,
    scope: &Scope,
    base: &Path,
) -> Result<Lowered, String> {
    let mut task = Task {
        image: Some(DEFAULT_IMAGE.to_owned()),
        command: Vec::new(),
        shell: false,
        captures: Vec::new(),
        success_codes: Vec::new(),
        network: false,
        stdout: None,
        stderr: None,
        workdir: Some(runtime::OUTDIR.into()),
        staging: Some(runtime::STAGING.into()),
        ..Task::default()
    };
    let attributes = requirements::apply(scope, &mut task)?;
    let named = crate::types::named(scope);
    let runtime = runtime::object(scope, process.version);
    let javascript = (scope.requirements.iter().chain(&scope.hints))
        .any(|entry| matches!(entry.class, RequirementClass::InlineJavascript { .. }));
    let names: Vec<&str> = (process.inputs.iter())
        .map(|input| short_name(&input.id))
        .collect();
    // Expressions see no listings, which CWL loads into Directory inputs.
    let listed = scope.find(|class| match class {
        RequirementClass::LoadListing { load_listing } => Some(*load_listing),
        _ => None,
    });
    let listed = listed
        .flatten()
        .is_some_and(|listing| listing != LoadListing::NoListing)
        || process
            .inputs
            .iter()
            .any(|input| input.load_listing.is_some());
    let runtime_of = |code: &Expression| {
        if listed && code.text.contains("listing") {
            return Err("an expression that reads a listing that CWL loads".to_owned());
        }
        // Without JavaScript, CWL refuses a reference to an input that the tool lacks.
        if !javascript && undeclared(code, &names) {
            return Err("a reference to an input that the tool does not declare".to_owned());
        }
        let strict;
        let code = match javascript {
            true => code,
            false => {
                strict = Expression::parse(&runtime::strict(code), false)?;
                &strict
            }
        };
        match &runtime {
            Ok(object) => Ok(runtime::with(code, object)),
            Err(why) if code.text.contains("runtime") => Err(why.clone()),
            Err(_) => Ok(code.text.clone()),
        }
    };
    let mut values = Vec::new();
    let mut stream = |field: Option<&Expression>, port: &str| match field {
        None => Ok(None),
        Some(text) if !code(text) => Ok(literal(Some(text), port)?.map(FileName::Text)),
        Some(text) if paths(text) => Err(format!("an expression in {port} that reads a path")),
        Some(text) => {
            values.push(Computed {
                port: port.to_owned(),
                code: runtime_of(text)?,
                this: None,
                r#type: None,
            });
            Ok(Some(FileName::Input {
                input: port.to_owned(),
            }))
        }
    };
    task.stdout = stream(tool.stdout.as_ref(), "stdout.name")?;
    task.stderr = stream(tool.stderr.as_ref(), "stderr.name")?;
    if let Some(listing) = scope.find(|class| match class {
        RequirementClass::InitialWorkDir { listing } => Some(listing),
        _ => None,
    }) {
        let code = |text: &Expression| match paths(text) {
            true => Err("an InitialWorkDir entry that reads a path".to_owned()),
            false => runtime_of(text),
        };
        task.files = crate::workdir::entries(listing, &names, &mut values, &code)?;
    }
    let streams = [
        (&mut task.stdout, Cwl::Stdout),
        (&mut task.stderr, Cwl::Stderr),
    ];
    for (name, kind) in streams {
        if name.is_none()
            && process
                .outputs
                .iter()
                .any(|output| output.value_type == kind)
        {
            *name = Some(FileName::Text(stream_name(&process.id, &kind)));
        }
    }
    task.stdin = match &tool.stdin {
        None => None,
        Some(text) if !code(text) => literal(Some(text), "stdin")?.map(Content::Text),
        // The path of a File input is where the task stages it.
        Some(text) => match text.parts.as_slice() {
            [Part::Reference(code)] => (code.strip_prefix("inputs."))
                .and_then(|rest| rest.strip_suffix(".path"))
                .filter(|name| names.contains(name))
                .map(|name| Content::Input {
                    input: name.to_owned(),
                }),
            _ => None,
        }
        .map(Some)
        .ok_or("stdin set by an expression")?,
    };
    if let Some(input) = (process.inputs.iter()).find(|input| input.value_type == Cwl::Stdin) {
        let input = short_name(&input.id).to_owned();
        task.stdin = Some(Content::Input { input });
    }
    for (name, value) in [("HOME", runtime::OUTDIR), ("TMPDIR", runtime::TMPDIR)] {
        task.environment
            .insert(name.into(), Content::Text(value.into()));
    }
    if let Some(variables) = scope.find(|class| match class {
        RequirementClass::EnvVar { variables } => Some(variables),
        _ => None,
    }) {
        for variable in variables {
            let name = &variable.env_name;
            let value = match code(&variable.env_value) {
                false => {
                    Content::Text(literal(Some(&variable.env_value), name)?.unwrap_or_default())
                }
                true if paths(&variable.env_value) => {
                    return Err(format!("the variable {name} that reads a path"));
                }
                true => {
                    let port = format!("environment.{name}");
                    values.push(Computed {
                        port: port.clone(),
                        code: runtime_of(&variable.env_value)?,
                        this: None,
                        r#type: None,
                    });
                    Content::Input { input: port }
                }
            };
            task.environment.insert(name.clone(), value);
        }
    }
    // CWL takes 0 as success unless a fail code names it, and every success code.
    let fails = tool
        .temporary_fail_codes
        .iter()
        .chain(&tool.permanent_fail_codes);
    let mut codes = tool.success_codes.clone();
    if !fails.clone().any(|code| *code == 0) && !codes.contains(&0) {
        codes.push(0);
    }
    if codes.is_empty() {
        return Err("fail codes that leave no success code".into());
    }
    task.success_codes = match codes.as_slice() {
        [0] => Vec::new(),
        _ => (codes.iter())
            .map(|code| i32::try_from(*code).map_err(|_| format!("the success code {code}")))
            .collect::<Result<_, _>>()?,
    };
    task.shell = (scope.requirements.iter().chain(&scope.hints))
        .any(|entry| matches!(entry.class, RequirementClass::ShellCommand));
    // In a shell command, a binding with `shellQuote: false` gives its words as they are.
    let quoted = |binding: Option<&CommandLineBinding>, words: Vec<Argument>| match task.shell
        && binding.is_some_and(|binding| !binding.shell_quote)
    {
        true => words.into_iter().map(unquoted).collect(),
        false => words,
    };
    let mut words: Vec<(Vec<Key>, Vec<Argument>)> = Vec::new();
    for (index, word) in tool.base_command.iter().enumerate() {
        let key = vec![Key::Number(-1_000_000), Key::Number(index as i64)];
        words.push((key, vec![Argument::Text(word.clone())]));
    }
    for (index, argument) in tool.arguments.iter().enumerate() {
        let (position, text, binding) = match argument {
            CwlArgument::Text(text) => (0, text.clone(), None),
            CwlArgument::Binding(binding) => match &binding.value_from {
                Some(text) => (position(binding)?, text.clone(), Some(binding)),
                None => continue,
            },
        };
        let key = vec![Key::Number(position), Key::Number(index as i64)];
        if code(&text) {
            // An expr sees where a file is otherwise than the tool, which sees it staged.
            if paths(&text) {
                return Err("an expression in an argument that reads a path".into());
            }
            let port = format!("argument.{index}");
            let word = Argument::Input {
                input: port.clone(),
                prefix: binding.and_then(|binding| binding.prefix.clone()),
                separate: binding.is_none_or(|binding| binding.separate),
                separator: binding.and_then(|binding| binding.item_separator.clone()),
                quote: true,
            };
            values.push(Computed {
                port,
                code: runtime_of(&text)?,
                this: None,
                r#type: None,
            });
            words.push((key, quoted(binding, vec![word])));
            continue;
        }
        let text = literal(Some(&text), "an argument")?.unwrap_or_default();
        let words_of = match binding {
            Some(binding) => prefixed(binding, Argument::Text(text))?,
            None => vec![Argument::Text(text)],
        };
        words.push((key, quoted(binding, words_of)));
    }
    let mut inputs = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        // CWL reads the contents and refuses files over 64 KiB, which a task does not.
        if input.load_contents == Some(true) {
            return Err(format!("loadContents of input {name}"));
        }
        if let Some(binding) = &input.binding {
            let key = vec![Key::Number(position(binding)?), Key::Name(name.clone())];
            let resolved = crate::types::resolve(&input.value_type, &named);
            words.push((key, quoted(Some(binding), bound(binding, resolved, &name)?)));
            if let Some(code) = &binding.value_from {
                values.push(Computed {
                    port: format!("{name}.{}", crate::group::VALUE),
                    code: runtime_of(code)?,
                    this: Some(name.clone()),
                    r#type: None,
                });
            }
        }
        let r#type = convert(&input.value_type, &named)
            .and_then(|r#type| crate::types::formatted(r#type, &input.format, &named))
            .and_then(|r#type| crate::types::secondary(r#type, &input.secondary_files, true))
            .map_err(|why| format!("input {name}: {why}"))?;
        let mut port = Port {
            name,
            r#type,
            flow: wir::Flow::Value,
            default: None,
        };
        if let Some(default) = &input.default {
            defaults::set(&mut port, default, base)
                .map_err(|why| format!("input {}: {why}", port.name))?;
        }
        inputs.push(port);
    }
    words.sort_by(|a, b| compare(&a.0, &b.0));
    task.command = words.into_iter().flat_map(|(_, words)| words).collect();
    let mut outputs = Vec::new();
    let mut evaluated = Vec::new();
    let mut glob = |name: &str, globs: &[Expression]| match globs {
        [one] if !code(one) => Ok(CaptureRule::Glob(pattern(one)?)),
        _ if globs.iter().any(paths) => Err("an expression in a glob that reads a path".into()),
        // The output folder itself is the Directory `.`, which a capture does not fill.
        [one] if one.text.trim() == "$(runtime.outdir)" => {
            Err("the glob of the output folder".into())
        }
        _ => {
            for literal in globs.iter().filter(|item| !code(item)) {
                pattern(literal)?;
            }
            let code = match globs {
                [one] => runtime_of(one)?,
                many => runtime_of(&Expression::parse(&runtime::list(many), false)?)?,
            };
            let port = format!("glob.{name}");
            values.push(Computed {
                port: port.clone(),
                code,
                this: None,
                r#type: None,
            });
            Ok(CaptureRule::Patterns(port))
        }
    };
    for output in &process.outputs {
        let name = short_name(&output.id).to_owned();
        let port = |r#type| Port {
            name: name.clone(),
            r#type,
            flow: wir::Flow::Value,
            default: None,
        };
        let (files, code) = match capture(output, &task, (listed, &named), &mut glob) {
            Ok(Read::Direct(rule, r#type)) => (Some((rule, r#type)), None),
            Ok(Read::Evaluated(files, code)) => (files, Some(code)),
            Err(why) => return Err(format!("output {name}: {why}")),
        };
        if let Some(code) = code {
            let r#type = convert(&output.value_type, &named)?;
            evaluated.push(Evaluated {
                output: port(r#type),
                code: Expression::parse(&runtime_of(&code)?, false)?,
                captured: files.is_some(),
            });
        }
        if let Some((rule, r#type)) = files {
            task.captures.push(Capture {
                output: name.clone(),
                from: rule,
                load_contents: loads(output),
                optional: false,
                arity: None,
            });
            outputs.push(port(r#type));
        }
    }
    Ok(Lowered {
        task,
        inputs,
        outputs,
        attributes,
        values,
        evaluated,
    })
}

/// The words of an input binding: the value of the input, after its prefix.
fn bound(
    binding: &CommandLineBinding,
    value_type: &Cwl,
    name: &str,
) -> Result<Vec<Argument>, String> {
    let input = |input: String, separator: Option<&String>| Argument::Input {
        input,
        prefix: binding.prefix.clone(),
        separate: binding.separate,
        separator: separator.cloned(),
        quote: true,
    };
    if binding.value_from.is_some() {
        // CWL binds no input that is null, where an expr of its valueFrom still runs.
        return match value_type {
            Cwl::Union(_) => Err(format!("the valueFrom of input {name}")),
            _ => Ok(vec![input(format!("{name}.{}", crate::group::VALUE), None)]),
        };
    }
    let members: Vec<&Cwl> = match value_type {
        Cwl::Union(members) => members
            .iter()
            .filter(|member| **member != Cwl::Null)
            .collect(),
        other => vec![other],
    };
    let optional = matches!(value_type, Cwl::Union(members) if members.contains(&Cwl::Null));
    match members.as_slice() {
        // CWL writes no word for a bool without a prefix.
        [Cwl::Boolean] if binding.prefix.is_none() => Ok(Vec::new()),
        [Cwl::Boolean] => Ok(vec![input(name.to_owned(), None)]),
        [Cwl::Array(array)] if array.binding.is_none() && plain(&array.items) => {
            let separator = binding.item_separator.as_ref();
            match (&binding.prefix, separator) {
                (None, None) => Ok(vec![Argument::input(name)]),
                _ => Ok(vec![input(name.to_owned(), separator)]),
            }
        }
        [single] if plain(single) && !optional && binding.separate => {
            prefixed(binding, Argument::input(name))
        }
        [single] if plain(single) => Ok(vec![input(name.to_owned(), None)]),
        _ => Err(format!("the command line binding of input {name}")),
    }
}

/// Types whose value is one word on the command line in CWL and in wir alike.
fn plain(r#type: &Cwl) -> bool {
    match r#type {
        Cwl::Int | Cwl::Long | Cwl::Float | Cwl::Double | Cwl::String | Cwl::File => true,
        Cwl::Directory => true,
        Cwl::Enum(schema) => schema.binding.is_none(),
        _ => false,
    }
}

/// A word after the prefix of a binding, as a separate word.
fn prefixed(binding: &CommandLineBinding, word: Argument) -> Result<Vec<Argument>, String> {
    match (&binding.prefix, binding.separate) {
        (None, _) => Ok(vec![word]),
        (Some(prefix), true) => Ok(vec![Argument::Text(prefix.clone()), word]),
        (Some(prefix), false) => match word {
            Argument::Text(text) => Ok(vec![Argument::Text(format!("{prefix}{text}"))]),
            _ => Err("a prefix joined to an input value".into()),
        },
    }
}

fn position(binding: &CommandLineBinding) -> Result<i64, String> {
    match &binding.position {
        None => Ok(0),
        Some(Dynamic::Value(position)) => Ok(*position),
        Some(Dynamic::Expression(_)) => Err("a position set by an expression".into()),
    }
}

/// The text of an expression without code; `what` names it in the reason.
fn literal(expression: Option<&Expression>, what: &str) -> Result<Option<String>, String> {
    let Some(expression) = expression else {
        return Ok(None);
    };
    let mut text = String::new();
    for part in &expression.parts {
        match part {
            Part::Text(piece) => text.push_str(piece),
            _ => return Err(format!("an expression in {what}")),
        }
    }
    Ok(Some(text))
}

/// How a task reads an output, and the output's type.
fn capture(
    output: &cwl_parser::model::OutputParameter,
    task: &Task,
    (listed, named): (bool, &crate::types::Named),
    glob: &mut Globs<'_>,
) -> Result<Read, String> {
    let name = short_name(&output.id);
    let file = |format: Option<String>| {
        Type::File(FileType {
            format,
            secondary_files: Vec::new(),
        })
    };
    // A format that code computes is set on each File by an expr after the task.
    let computed = output.format.as_ref().filter(|format| code(format));
    if computed.is_some_and(paths) {
        return Err("an expression in the format that reads a path".into());
    }
    let read = |rule: CaptureRule, r#type: Type| match computed {
        Some(format) => Ok(Read::Evaluated(
            Some((rule, r#type)),
            Expression::parse(&runtime::formats(format), false)?,
        )),
        None => Ok(Read::Direct(rule, r#type)),
    };
    let format = match computed {
        Some(_) => None,
        None => literal(output.format.as_ref(), "the format")?,
    };
    let secondary = !output.secondary_files.is_empty();
    match &output.value_type {
        Cwl::Stdout | Cwl::Stderr if secondary => return Err("secondary files of a stream".into()),
        Cwl::Stdout if output.binding.is_none() => return read(CaptureRule::Stdout, file(format)),
        Cwl::Stderr if output.binding.is_none() => return read(CaptureRule::Stderr, file(format)),
        _ => {}
    }
    let Some(binding) = &output.binding else {
        return Err("no glob".into());
    };
    if binding.load_listing.is_some() {
        return Err("loadListing".into());
    }
    if let Some(code) = &binding.output_eval {
        if output.format.is_some() || secondary {
            return Err("the format or secondary files of an evaluated output".into());
        }
        let reads = paths(code) || code.text.contains("exitCode");
        if reads || (listed && code.text.contains("listing")) {
            return Err("an outputEval that reads the exit code, a path or a listing".into());
        }
        let files = match binding.glob.as_slice() {
            [] => None,
            globs => {
                let rule = glob(name, globs)?;
                let items = match holds(&output.value_type, &Cwl::Directory)
                    && !holds(&output.value_type, &Cwl::File)
                {
                    true => Type::Directory,
                    false => file(None),
                };
                let found = Type::Array(wir::ArrayType {
                    items: Box::new(items),
                    nonempty: false,
                });
                Some((rule, found))
            }
        };
        return Ok(Read::Evaluated(files, code.clone()));
    }
    if binding.glob.is_empty() {
        return Err("no glob".into());
    }
    let rule = match glob(name, &binding.glob)? {
        CaptureRule::Glob(pattern) => self::rule(&pattern, task),
        other => other,
    };
    let r#type = convert(&output.value_type, named)?;
    let r#type = with_format(r#type, format);
    let r#type = crate::types::secondary(r#type, &output.secondary_files, false)?;
    let fits = match &r#type {
        Type::Optional(inner) => matches!(**inner, Type::File(_) | Type::Directory),
        Type::Array(array) => matches!(*array.items, Type::File(_) | Type::Directory),
        other => matches!(other, Type::File(_) | Type::Directory),
    };
    match fits {
        true => read(rule, r#type),
        false => Err("a type that a glob does not fill".into()),
    }
}

/// Whether an output loads the contents of the files of its glob.
fn loads(output: &cwl_parser::model::OutputParameter) -> bool {
    (output.binding.as_ref()).is_some_and(|binding| binding.load_contents == Some(true))
}

/// The pattern of a glob without code that stays inside the working folder.
fn pattern(glob: &Expression) -> Result<String, String> {
    let pattern = literal(Some(glob), "a glob")?.unwrap_or_default();
    if pattern.is_empty() || pattern.starts_with('/') || pattern == "." || pattern.contains("..") {
        return Err(format!("the glob {pattern:?}"));
    }
    Ok(pattern)
}

/// The capture of a glob; a stream file is also a file in the working folder, which a glob
/// may match.
fn rule(pattern: &str, task: &Task) -> CaptureRule {
    match task.stdout == Some(FileName::Text(pattern.to_owned())) {
        true => CaptureRule::Stdout,
        false => CaptureRule::Glob(pattern.to_owned()),
    }
}

/// The name uwe's CWL driver gives the stream file of a tool that names none.
fn stream_name(id: &str, kind: &Cwl) -> String {
    use sha1::Digest;
    let label = match kind {
        Cwl::Stderr => "stderr",
        _ => "stdout",
    };
    let digest = sha1::Sha1::digest(format!("{id}#{label}").as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}.{label}", &hex[..16])
}

/// A word that a shell command takes as it is.
fn unquoted(word: Argument) -> Argument {
    match word {
        Argument::Text(raw) => Argument::Raw { raw },
        Argument::Input {
            input,
            prefix,
            separate,
            separator,
            ..
        } => Argument::Input {
            input,
            prefix,
            separate,
            separator,
            quote: false,
        },
        other => other,
    }
}

/// Whether code reads or makes where files are, which an expr sees otherwise than a tool.
fn paths(code: &Expression) -> bool {
    let words = ["path", "dirname", "location"];
    words.iter().any(|word| code.text.contains(word))
}

/// Whether a parameter reference of an expression names an input that is not in `names`.
fn undeclared(code: &Expression, names: &[&str]) -> bool {
    code.parts.iter().any(|part| {
        let Part::Reference(text) = part else {
            return false;
        };
        let Some(rest) = text.strip_prefix("inputs") else {
            return false;
        };
        let name: String = match rest.strip_prefix('.') {
            Some(rest) => (rest.chars())
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect(),
            None => (rest.trim_start_matches(['[', '\'', '"']).chars())
                .take_while(|c| !matches!(c, '\'' | '"'))
                .collect(),
        };
        !name.is_empty() && !names.contains(&name.as_str())
    })
}

/// Whether an expression has code.
fn code(expression: &Expression) -> bool {
    (expression.parts.iter()).any(|part| !matches!(part, Part::Text(_)))
}

/// Whether a CWL type is `kind`, or holds it in a union or an array.
fn holds(r#type: &Cwl, kind: &Cwl) -> bool {
    match r#type {
        Cwl::Union(members) => members.iter().any(|member| holds(member, kind)),
        Cwl::Array(array) => holds(&array.items, kind),
        other => other == kind,
    }
}

/// The type with the format on its File, also inside an optional or an array.
fn with_format(r#type: Type, format: Option<String>) -> Type {
    match r#type {
        Type::File(_) => Type::File(FileType {
            format,
            secondary_files: Vec::new(),
        }),
        Type::Optional(inner) => Type::Optional(Box::new(with_format(*inner, format))),
        Type::Array(mut array) => {
            array.items = Box::new(with_format(*array.items, format));
            Type::Array(array)
        }
        other => other,
    }
}

/// Where a word goes on the command line, as cwltool sorts bindings.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    Number(i64),
    Name(String),
}

impl Key {
    fn text(&self) -> String {
        match self {
            Key::Number(number) => number.to_string(),
            Key::Name(name) => name.clone(),
        }
    }
}

/// Key lists compare element by element; a shorter list comes first, and a name compares as
/// text with anything, as in uwe's CWL driver.
fn compare(a: &[Key], b: &[Key]) -> Ordering {
    for index in 0..a.len().max(b.len()) {
        match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) if x == y => continue,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(Key::Number(x)), Some(Key::Number(y))) => return x.cmp(y),
            (Some(x), Some(y)) => {
                return match x.text() > y.text() {
                    true => Ordering::Greater,
                    false => Ordering::Less,
                };
            }
        }
    }
    Ordering::Equal
}
