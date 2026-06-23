//! Collecting the outputs of a finished task, as cwltool's `collect_output` does: from
//! `cwl.output.json`, or from the files that the globs of the output bindings match; in a
//! simulated run, outputs the executor could not make get placeholders.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::iri;
use cwl_parser::model::{
    Class, Dynamic, Expression, LoadListing, OutputBinding, OutputParameter, SecondaryFile, Type,
    short_name,
};
use workflow_graph::simulator::glob_matches;
use workflow_graph::{Outcome, OutputFile};

use super::outputs::{classes, globs};
use super::{OUTPUT_JSON, Pending, STAGING, TMPDIR};
use crate::expression::Variables;
use crate::files::{self, Access};
use crate::inputs::has_code;
use crate::placeholder;
use crate::value::{Map, Value};

impl Pending {
    /// Whether the task is an abstract `Operation`, which runs no tool.
    pub fn is_abstract(&self) -> bool {
        matches!(self.process.class, Class::Operation)
    }

    /// The output object of the task from its outcome.
    pub fn collect(self, outcome: &Outcome) -> Result<Map, String> {
        let exit = i64::from(outcome.exit_code);
        if exit != 0 && !self.success_codes.contains(&exit) {
            return Err(format!("the command ended with exit code {exit}"));
        }
        let mut runtime = self.runtime.clone();
        runtime.insert("exitCode".into(), Value::Integer(exit));
        let collector = Collector {
            pending: &self,
            outcome,
            runtime: Value::Map(runtime),
            access: self.options.access(),
            workdir: workdir(outcome),
        };
        let from_json = match outcome.files.iter().find(|file| file.path == OUTPUT_JSON) {
            Some(file) => {
                let text = std::fs::read_to_string(&file.location).unwrap_or_default();
                match Value::parse(&text) {
                    Ok(Value::Map(map)) => Some(map),
                    // The simulator makes an empty file, which gives no outputs.
                    _ if self.options.simulated => None,
                    Ok(_) => return Err(format!("{OUTPUT_JSON} must hold an object")),
                    Err(error) => return Err(format!("{OUTPUT_JSON}: {error}")),
                }
            }
            None => None,
        };
        let is_operation = !matches!(self.process.class, Class::CommandLineTool(_));
        let mut outputs = Map::new();
        if let Some(mut map) = from_json {
            for (_, value) in map.iter_mut() {
                collector.revmap_all(value)?;
            }
            let mut value = Value::Map(map);
            files::normalize(&mut value)?;
            let Value::Map(map) = value else {
                unreachable!("an object stays a map")
            };
            outputs = map;
            for (_, value) in outputs.iter_mut() {
                collector.checksums(value)?;
            }
        } else {
            for output in &self.process.outputs {
                let name = short_name(&output.id).to_owned();
                let collected = if is_operation {
                    collector.operation(output)
                } else if output.binding.is_none()
                    && !matches!(
                        self.types.resolve(&output.value_type),
                        Type::Stdout | Type::Stderr | Type::Record(_)
                    )
                {
                    // Only cwl.output.json gives this output; a simulated run has none.
                    Ok(Value::Null)
                } else {
                    collector.output(
                        &name,
                        &output.value_type,
                        output.binding.as_ref(),
                        &output.secondary_files,
                        output.format.as_ref(),
                    )
                };
                let value = collected.map_err(|error| format!("the output `{name}`: {error}"))?;
                outputs.insert(name, value);
            }
        }
        for output in &self.process.outputs {
            let name = short_name(&output.id).to_owned();
            let value = outputs.get(&name).cloned().unwrap_or(Value::Null);
            if !self.types.matches(&output.value_type, &value) {
                if self.options.simulated {
                    let placeholder = self.placeholder(output, &collector);
                    outputs.insert(name, placeholder);
                } else {
                    return Err(format!(
                        "the output `{name}` is of type {}, but its value is {}",
                        crate::inputs::describe(&output.value_type),
                        value.summary()
                    ));
                }
            }
        }
        for (_, value) in outputs.iter_mut() {
            files::visit_files(value, &mut |file| {
                file.shift_remove("path");
                file.shift_remove("dirname");
            });
        }
        Ok(outputs)
    }

    /// A value for an output the executor could not make, with files in its work folder.
    fn placeholder(&self, output: &OutputParameter, collector: &Collector<'_>) -> Value {
        let base = match &collector.workdir {
            Some(folder) => format!("{}/", iri::file_url(Path::new(folder))),
            None => "_:".to_owned(),
        };
        let name = short_name(&output.id);
        let hint = placeholder::hint_of(name, output.format.as_slice(), &output.secondary_files);
        let mut value = placeholder::value(&self.types, &output.value_type, name, &base, &hint);
        // Placeholder files point at the task's JSON output, so the graph keeps the link.
        if let Some(json) = collector
            .outcome
            .files
            .iter()
            .find(|file| file.path == OUTPUT_JSON)
        {
            let location = iri::file_url(&json.location);
            files::visit_files(&mut value, &mut |file| {
                if file.get("class").and_then(Value::as_str) == Some("File") {
                    let name = file
                        .get("location")
                        .and_then(Value::as_str)
                        .map(files::basename_of)
                        .unwrap_or_default();
                    file.insert("basename".into(), Value::String(name));
                    file.insert("location".into(), Value::String(location.clone()));
                }
            });
        }
        let _ = files::normalize(&mut value);
        let access = self.options.access();
        files::visit_files(&mut value, &mut |file| {
            if file.get("class").and_then(Value::as_str) == Some("File") {
                let _ = access.add_checksum(file);
            } else {
                access.load_listing(file, false);
            }
        });
        // The secondary files the output declares come with it, as with a real run.
        if !output.secondary_files.is_empty() {
            let _ = collector.secondary_files(&mut value, &output.secondary_files);
        }
        value
    }
}

/// The folder the executor put the task's outputs in, from any output file.
pub fn workdir(outcome: &Outcome) -> Option<String> {
    outcome.files.iter().find_map(|file| {
        let location = file.location.to_string_lossy();
        location
            .strip_suffix(file.path.trim_end_matches('/'))
            .map(|folder| folder.trim_end_matches('/').to_owned())
    })
}

/// Collects outputs from the files of an outcome.
struct Collector<'a> {
    pending: &'a Pending,
    outcome: &'a Outcome,
    runtime: Value,
    access: Access,
    workdir: Option<String>,
}

impl Collector<'_> {
    fn vars<'v>(&'v self, this: &'v Value) -> Variables<'v> {
        Variables {
            inputs: &self.pending.inputs,
            this,
            runtime: &self.runtime,
        }
    }

    fn evaluate(&self, expression: &Expression, this: &Value) -> Result<Value, String> {
        self.pending.evaluator.evaluate(expression, self.vars(this))
    }

    /// Gives every file object of a value its host location, as cwltool's `revmap_file`.
    fn revmap_all(&self, value: &mut Value) -> Result<(), String> {
        match value {
            Value::List(items) => {
                for item in items {
                    self.revmap_all(item)?;
                }
                Ok(())
            }
            Value::Map(_) if value.is_file_like() => {
                self.revmap(value)?;
                if let Value::Map(map) = value {
                    for key in ["secondaryFiles", "listing"] {
                        if let Some(inner) = map.get_mut(key) {
                            self.revmap_all(inner)?;
                        }
                    }
                }
                Ok(())
            }
            Value::Map(map) => {
                for (_, item) in map.iter_mut() {
                    self.revmap_all(item)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn revmap(&self, value: &mut Value) -> Result<(), String> {
        let Value::Map(map) = value else {
            return Ok(());
        };
        let location = map
            .get("location")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut path = map.get("path").and_then(Value::as_str).map(str::to_owned);
        // A `file:` location inside the output directory of the container, as
        // `cwl.output.json` may give one, names a path there.
        let outdir = &self.pending.outdir;
        if path.is_none()
            && let Some(file_path) = location.as_deref().and_then(iri::file_path)
            && file_path.starts_with(outdir)
        {
            path = Some(file_path.to_string_lossy().into_owned());
        }
        match (location, path) {
            (Some(location), None) => {
                if !iri::has_scheme(&location) && !location.starts_with("_:") {
                    let base = self.workdir.clone().unwrap_or_default();
                    let joined = iri::file_url(&Path::new(&base).join(&location));
                    map.insert("location".into(), Value::String(joined));
                }
                Ok(())
            }
            (_, Some(path)) => {
                map.shift_remove("dirname");
                let outdir = &self.pending.outdir;
                let absolute = if path.starts_with('/') {
                    path.clone()
                } else {
                    format!("{outdir}/{path}")
                };
                map.shift_remove("path");
                if !map.contains_key("basename") {
                    map.insert(
                        "basename".into(),
                        Value::String(files::basename_of(&absolute)),
                    );
                }
                let location = if let Some(original) = self.pending.staged.get(&absolute) {
                    original.clone()
                } else if absolute == *outdir || absolute.starts_with(&format!("{outdir}/")) {
                    let relative = absolute[outdir.len()..].trim_start_matches('/');
                    let base = self.workdir.clone().unwrap_or_default();
                    iri::file_url(&Path::new(&base).join(relative))
                } else if absolute.starts_with(TMPDIR) || absolute.starts_with(STAGING) {
                    return Err(format!(
                        "the output `{path}` is outside the output directory"
                    ));
                } else {
                    // A path from the host, such as a file in a work folder of the simulator.
                    iri::file_url(Path::new(&absolute))
                };
                map.insert("location".into(), Value::String(location));
                // Keep the container path for expressions that come after, like secondaryFiles.
                map.insert("path".into(), Value::String(absolute));
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    /// Adds size and checksum to the files of a value that lack them.
    fn checksums(&self, value: &mut Value) -> Result<(), String> {
        let access = self.access;
        let mut error = None;
        files::visit_files(value, &mut |file| {
            let is_file = file.get("class").and_then(Value::as_str) == Some("File");
            if is_file
                && !file.contains_key("checksum")
                && let Err(message) = access.add_checksum(file)
            {
                error.get_or_insert(message);
            }
        });
        match error {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    /// The value of an output of an `Operation`: the file named after it, or a placeholder in
    /// a simulated run. A real run makes no files for an `Operation`, which is a task that does
    /// nothing, so its outputs are null, and an error when their type does not allow null.
    fn operation(&self, output: &OutputParameter) -> Result<Value, String> {
        let name = short_name(&output.id);
        let types = &self.pending.types;
        let found = self.matches(name);
        let binding = OutputBinding {
            glob: Vec::new(),
            load_contents: None,
            load_listing: None,
            output_eval: None,
        };
        let mut file = match found.first() {
            Some(file) => self.file_object(file, &binding)?,
            None if self.pending.options.simulated => {
                return Ok(self.pending.placeholder(output, self));
            }
            None if types.optional(&output.value_type) => return Ok(Value::Null),
            None => {
                return Err(
                    "the step runs an abstract Operation, which has no tool, so a real \
                            run has no value for this output; give the step a tool that makes it"
                        .into(),
                );
            }
        };
        if !output.secondary_files.is_empty() {
            self.secondary_files(&mut file, &output.secondary_files)?;
        }
        let mut value_type = types.resolve(&output.value_type);
        let mut wrap = 0;
        loop {
            match value_type {
                Type::Array(array) => {
                    wrap += 1;
                    value_type = types.resolve(&array.items);
                }
                Type::Union(members) => {
                    match members.iter().find(|member| !matches!(member, Type::Null)) {
                        Some(member) => value_type = types.resolve(member),
                        None => break,
                    }
                }
                _ => break,
            }
        }
        let mut value = file;
        for _ in 0..wrap {
            value = Value::List(vec![value]);
        }
        let mut value = value;
        files::visit_files(&mut value, &mut |file| {
            file.shift_remove("path");
        });
        Ok(value)
    }

    /// cwltool's `collect_output` for one output or record field.
    fn output(
        &self,
        name: &str,
        value_type: &Type,
        binding: Option<&OutputBinding>,
        secondary: &[SecondaryFile],
        format: Option<&Expression>,
    ) -> Result<Value, String> {
        let types = &self.pending.types;
        let resolved = types.resolve(value_type);
        let binding = match (binding, resolved) {
            (Some(binding), _) => Some(binding.clone()),
            (None, Type::Stdout | Type::Stderr) => {
                let name = if matches!(resolved, Type::Stdout) {
                    &self.pending.stdout
                } else {
                    &self.pending.stderr
                };
                name.as_ref().map(|name| OutputBinding {
                    glob: vec![Expression {
                        text: name.clone(),
                        parts: vec![cwl_parser::model::Part::Text(name.clone())],
                    }],
                    load_contents: None,
                    load_listing: None,
                    output_eval: None,
                })
            }
            _ => None,
        };
        let Some(binding) = binding else {
            if let Type::Record(record) = resolved {
                let mut map = Map::new();
                for field in &record.fields {
                    let value = self.output(
                        &field.name,
                        &field.value_type,
                        field.output_binding.as_ref(),
                        &field.secondary_files,
                        field.format.first(),
                    )?;
                    map.insert(field.name.clone(), value);
                }
                return Ok(Value::Map(map));
            }
            return Ok(Value::Null);
        };
        let null = Value::Null;
        let patterns = globs(
            &binding,
            &self.pending.evaluator,
            self.vars(&null),
            &self.pending.outdir,
        )?;
        // Without `outputEval` the matches are the value, so they must be of a class the
        // output takes, as cwltool's type check of outputs finds.
        let (files, directories) = classes(types, value_type);
        let checked = binding.output_eval.is_none() && files != directories;
        let mut found: Vec<Value> = Vec::new();
        for pattern in &patterns {
            for file in self.matches(pattern) {
                if checked && file.directory != directories {
                    let (class, taken) = if file.directory {
                        ("directory", "files")
                    } else {
                        ("file", "directories")
                    };
                    return Err(format!(
                        "the glob `{pattern}` matches the {class} `{}`, but the output takes {taken}",
                        file.path
                    ));
                }
                self.check_link(&file)?;
                let mut object = self.file_object(&file, &binding)?;
                self.simulated_extension(&mut object, &file, pattern, name, format, secondary);
                found.push(object);
            }
        }
        let (optional, single) = match resolved {
            Type::Union(members) => (
                members.contains(&Type::Null),
                members
                    .iter()
                    .any(|member| matches!(member, Type::File | Type::Directory)),
            ),
            Type::File | Type::Directory | Type::Stdout | Type::Stderr => (false, true),
            _ => (false, false),
        };
        let mut result = match &binding.output_eval {
            Some(expression) => match self.evaluate(expression, &Value::List(found)) {
                Ok(value) => value,
                // Expressions that read the files of a simulated run read empty files and may
                // fail; the output then gets a placeholder, as when it does not fit its type.
                Err(_) if self.access.assume_present && reads_self(expression) => Value::Null,
                Err(error) => return Err(error),
            },
            None => Value::List(found),
        };
        if single {
            match result {
                Value::List(items) if items.is_empty() => {
                    if !optional && !self.access.assume_present {
                        return Err(format!("no output file matches {patterns:?}"));
                    }
                    result = Value::Null;
                }
                Value::List(mut items) if items.len() == 1 => result = items.remove(0),
                Value::List(items) if !self.access.assume_present => {
                    return Err(format!(
                        "{} files match {patterns:?}, where one is expected",
                        items.len()
                    ));
                }
                Value::List(mut items) => result = items.remove(0),
                other => result = other,
            }
        }
        if !secondary.is_empty() {
            self.secondary_files(&mut result, secondary)?;
        }
        if let Some(format) = format {
            let mut primaries: Vec<&mut Value> = match &mut result {
                Value::List(items) => items.iter_mut().collect(),
                other => vec![other],
            };
            for primary in primaries.iter_mut() {
                if !primary.is_file() {
                    continue;
                }
                let value = match self.evaluate(format, primary) {
                    Ok(value) => value,
                    // The format is metadata; over the inputs of a simulated run it may not exist.
                    Err(_) if self.access.assume_present => Value::Null,
                    Err(error) => return Err(error),
                };
                if let (Value::Map(map), Value::String(format)) = (&mut **primary, value) {
                    map.insert("format".into(), Value::String(format));
                }
            }
        }
        self.revmap_all(&mut result)?;
        // cwltool keeps 0 and "" but turns other empty results of optional outputs into null.
        let kept = matches!(
            result,
            Value::Integer(0) | Value::Bool(false) | Value::String(_)
        ) || matches!(result, Value::Float(number) if number == 0.0);
        if optional && !result.is_truthy() && !kept {
            return Ok(Value::Null);
        }
        Ok(result)
    }

    /// The outcome files a glob pattern matches, sorted, with `.` for the work folder.
    fn matches(&self, pattern: &str) -> Vec<OutputFile> {
        if pattern == "." {
            return match &self.workdir {
                Some(folder) => vec![OutputFile {
                    path: ".".into(),
                    location: folder.into(),
                    directory: true,
                }],
                None => Vec::new(),
            };
        }
        let mut found: Vec<OutputFile> = Vec::new();
        for file in &self.outcome.files {
            let path = file.path.trim_end_matches('/');
            if glob_matches(pattern, path) {
                found.push(OutputFile {
                    path: path.to_owned(),
                    location: file.location.clone(),
                    directory: file.directory || file.location.is_dir(),
                });
            } else if file.directory || file.location.is_dir() {
                // A folder the executor gave back may hold what the pattern names.
                for (relative, location, directory) in walk(&file.location, path) {
                    if glob_matches(pattern, &relative) {
                        found.push(OutputFile {
                            path: relative,
                            location,
                            directory,
                        });
                    }
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found.dedup_by(|a, b| a.path == b.path);
        found
    }

    /// In a simulated run, the file that the simulator names after a bare wildcard, like `mock`
    /// for `*`, has no extension; its name takes the one the output suggests, as the tool's
    /// file has.
    fn simulated_extension(
        &self,
        object: &mut Value,
        file: &OutputFile,
        pattern: &str,
        name: &str,
        format: Option<&Expression>,
        secondary: &[SecondaryFile],
    ) {
        let generic = workflow_graph::simulator::file_name(pattern);
        let bare = generic != pattern && files::split_extension(&generic).1.is_empty();
        if !self.access.assume_present || !bare || file.path != generic {
            return;
        }
        let (Value::Map(map), Some(extension)) = (
            object,
            placeholder::suggested_extension(name, format, secondary),
        ) else {
            return;
        };
        map.insert(
            "basename".into(),
            Value::String(format!("{generic}{extension}")),
        );
        map.insert("nameroot".into(), Value::String(generic));
        map.insert("nameext".into(), Value::string(extension));
    }

    /// Fails for a matched file that is a symbolic link to a place outside the output
    /// directory, which the task cannot give back.
    fn check_link(&self, file: &OutputFile) -> Result<(), String> {
        let Ok(target) = std::fs::read_link(&file.location) else {
            return Ok(());
        };
        let outdir = Path::new(&self.pending.outdir);
        let inside = if target.is_absolute() {
            target.starts_with(outdir)
                || self
                    .workdir
                    .as_ref()
                    .is_some_and(|folder| target.starts_with(folder))
        } else {
            // A relative target may climb at most to the output directory.
            let mut depth = Path::new(&file.path).components().count() as isize - 1;
            target.components().all(|part| {
                match part {
                    std::path::Component::ParentDir => depth -= 1,
                    std::path::Component::Normal(_) => depth += 1,
                    _ => {}
                }
                depth >= 0
            })
        };
        if inside {
            return Ok(());
        }
        Err(format!(
            "the output `{}` links to `{}`, outside the output directory",
            file.path,
            target.display()
        ))
    }

    /// The file object of a matched file, with contents, listing and checksum as asked.
    fn file_object(&self, file: &OutputFile, binding: &OutputBinding) -> Result<Value, String> {
        let class = if file.directory { "Directory" } else { "File" };
        let name = Path::new(&file.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| files::basename_of(&file.location.to_string_lossy()));
        let mut map = Map::new();
        map.insert("class".into(), Value::string(class));
        map.insert(
            "location".into(),
            Value::String(iri::file_url(&file.location)),
        );
        let path = if file.path == "." {
            self.pending.outdir.clone()
        } else {
            format!("{}/{}", self.pending.outdir, file.path)
        };
        map.insert("path".into(), Value::String(path));
        map.insert("basename".into(), Value::String(name.clone()));
        if file.directory {
            let listing = binding.load_listing.or(self.pending.load_listing);
            if let Some(listing) = listing
                && listing != LoadListing::NoListing
            {
                self.access
                    .load_listing(&mut map, listing == LoadListing::DeepListing);
                let mut listing_value = map
                    .shift_remove("listing")
                    .unwrap_or(Value::List(Vec::new()));
                self.checksums(&mut listing_value)?;
                map.insert("listing".into(), listing_value);
            }
        } else {
            let (root, extension) = files::split_extension(&name);
            map.insert("nameroot".into(), Value::string(root));
            map.insert("nameext".into(), Value::string(extension));
            if binding.load_contents == Some(true) {
                self.access.load_contents(&mut map)?;
            }
            self.access.add_checksum(&mut map)?;
        }
        Ok(Value::Map(map))
    }

    /// Adds the secondary files of output files that exist.
    fn secondary_files(
        &self,
        result: &mut Value,
        secondary: &[SecondaryFile],
    ) -> Result<(), String> {
        let mut primaries: Vec<&mut Value> = match result {
            Value::List(items) => items.iter_mut().collect(),
            other => vec![other],
        };
        for primary in primaries.iter_mut() {
            let Value::Map(map) = &mut **primary else {
                continue;
            };
            if !map.contains_key("secondaryFiles") {
                map.insert("secondaryFiles".into(), Value::List(Vec::new()));
            }
            let path = map
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let prefix = match path.rfind('/') {
                Some(slash) => path[..=slash].to_owned(),
                None => String::new(),
            };
            for entry in secondary {
                let this = Value::Map(map.clone());
                let required = match &entry.required {
                    None => false,
                    Some(Dynamic::Value(required)) => *required,
                    Some(Dynamic::Expression(expression)) => {
                        self.evaluate(expression, &this)?.is_truthy()
                    }
                };
                let names = if has_code(&entry.pattern) {
                    self.evaluate(&entry.pattern, &this)?
                } else {
                    let basename = map
                        .get("basename")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Value::String(files::substitute(basename, &entry.pattern.text))
                };
                let names = match names {
                    Value::List(items) => items,
                    Value::Null => Vec::new(),
                    other => vec![other],
                };
                for name in names {
                    let item = match name {
                        Value::String(name) if name.is_empty() => continue,
                        Value::String(name) => {
                            let mut item = Map::new();
                            item.insert("path".into(), Value::String(format!("{prefix}{name}")));
                            item
                        }
                        Value::Map(item) => item,
                        _ => continue,
                    };
                    let mut item = Value::Map(item);
                    self.revmap(&mut item)?;
                    let location = item.text("location").unwrap_or_default().to_owned();
                    let on_disk = files::local_path(&location);
                    let exists = on_disk.as_ref().is_some_and(|path| path.exists());
                    let is_directory = on_disk.as_ref().is_some_and(|path| path.is_dir());
                    if !exists && !self.access.assume_present {
                        if required {
                            return Err(format!(
                                "the required secondary file `{location}` is missing"
                            ));
                        }
                        continue;
                    }
                    if let Value::Map(item_map) = &mut item {
                        item_map.shift_remove("path");
                        let class = if is_directory { "Directory" } else { "File" };
                        item_map.insert("class".into(), Value::string(class));
                        if !is_directory {
                            let _ = self.access.add_checksum(item_map);
                        }
                    }
                    files::normalize(&mut item)?;
                    if let Some(Value::List(list)) = map.get_mut("secondaryFiles") {
                        list.push(item);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether an expression reads `self`, the files an output binding found.
fn reads_self(expression: &Expression) -> bool {
    expression.parts.iter().any(|part| match part {
        cwl_parser::model::Part::Text(_) => false,
        cwl_parser::model::Part::Reference(code)
        | cwl_parser::model::Part::Javascript(code)
        | cwl_parser::model::Part::Function(code) => code.contains("self"),
    })
}

/// The files and folders below a folder, relative to `prefix`.
fn walk(folder: &Path, prefix: &str) -> Vec<(String, std::path::PathBuf, bool)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = if prefix.is_empty() || prefix == "." {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let directory = path.is_dir();
        if directory {
            found.extend(walk(&path, &relative));
        }
        found.push((relative, path, directory));
    }
    found
}
