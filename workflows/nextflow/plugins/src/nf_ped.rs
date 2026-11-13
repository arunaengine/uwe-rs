//! nf-ped 0.4.0: reads PED pedigree files into an object that scripts query, edit and write.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use md5::Digest;
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::regex::Pattern;
use nextflow_evaluator::value::{Native, PathValue, Value};

use crate::catalogue::{Kind, Plugin, State};
use crate::{Context, native, object};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["initializePed"],
    call: Some(call),
    methods: Some(methods),
    ..Plugin::new("nf-ped", Kind::Functions, State::Implemented)
};

/// A pedigree or one of its entries.
#[derive(Debug)]
enum Ped {
    /// `nvnieuwk.ped.Ped`: its entries, a `LinkedHashSet` of entry objects that scripts may
    /// change through `getEntries()`.
    Pedigree(Value),
    /// `nvnieuwk.ped.PedEntry`
    Entry(Entry),
}

/// A line of a PED file. A missing ID is `None`; sex and phenotype are `0`, `1` or `2`.
#[derive(Clone, Debug)]
struct Entry {
    family: Option<String>,
    individual: Option<String>,
    father: Option<String>,
    mother: Option<String>,
    sex: &'static str,
    phenotype: &'static str,
    additional: Vec<String>,
}

impl Native for Ped {
    fn plugin(&self) -> &'static str {
        "nf-ped"
    }

    fn text(&self) -> String {
        match self {
            Ped::Pedigree(_) => format!("nvnieuwk.ped.Ped@{:x}", self as *const Ped as usize),
            Ped::Entry(entry) => entry.text(),
        }
    }
}

impl Entry {
    fn text(&self) -> String {
        let parent = |id: &Option<String>| id.clone().unwrap_or_else(|| "0".into());
        let id = |id: &Option<String>| id.clone().unwrap_or_else(|| "null".into());
        let mut fields = vec![
            id(&self.family),
            id(&self.individual),
            parent(&self.father),
            parent(&self.mother),
            self.sex.into(),
            self.phenotype.into(),
        ];
        fields.extend(self.additional.iter().cloned());
        fields.join("\t")
    }
}

/// An ID of a PED file: `0`, `NA` and the empty text mean none.
fn id(text: &str) -> Option<String> {
    (!matches!(text, "0" | "NA" | "")).then(|| text.to_owned())
}

/// A sex or phenotype code: `1` and `2` stay, anything else is `0`.
fn code(text: &str) -> &'static str {
    match text {
        "1" => "1",
        "2" => "2",
        _ => "0",
    }
}

fn entry_value(entry: Entry) -> Value {
    object("nvnieuwk.ped.PedEntry", Ped::Entry(entry))
}

fn text_value(text: &Option<String>) -> Value {
    text.as_deref().map_or(Value::Null, Value::string)
}

/// `initializePed([options,] [path or paths])`: a pedigree with the files imported in order.
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (options, files) = match arguments.as_slice() {
        [options @ Value::Map(_), rest @ ..] => (Some(options.clone()), rest.to_vec()),
        rest => (None, rest.to_vec()),
    };
    let paths = match files.as_slice() {
        [] => Vec::new(),
        [path @ Value::Path(_)] => vec![path.clone()],
        [Value::List(list)] if list.borrow().items.iter().all(is_path) => {
            list.borrow().items.clone()
        }
        _ => return Err(no_signature(name, &arguments)),
    };
    let pedigree = object("nvnieuwk.ped.Ped", Ped::Pedigree(Value::set([])));
    for path in paths {
        import(&pedigree, options.as_ref(), &path, context)?;
    }
    Ok(pedigree)
}

fn is_path(value: &Value) -> bool {
    matches!(value, Value::Path(_))
}

fn no_signature(name: &str, arguments: &[Value]) -> Error {
    let types: Vec<&str> = arguments.iter().map(Value::class_name).collect();
    let values: Vec<String> = arguments.iter().map(Value::to_string).collect();
    let message = format!(
        "No signature of method: nvnieuwk.ped.PedExtension.{name}() is applicable for argument \
         types: ({}) values: [{}]\nPossible solutions: initializePed(), \
         initializePed(java.nio.file.Path), initializePed(java.util.Map), \
         initializePed(java.util.Map, java.nio.file.Path), initializePed(java.util.Map, \
         java.util.List)",
        types.join(", "),
        values.join(", ")
    );
    thrown(Exception::new("MissingMethodException", message))
}

/// Runs a method of a pedigree or an entry, `None` for a method it does not have.
fn methods(
    object: &Value,
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Option<Value>, Error> {
    let entries = match native::<Ped>(object).as_deref() {
        Some(Ped::Pedigree(entries)) => Some(entries.clone()),
        Some(Ped::Entry(_)) => None,
        None => return Ok(None),
    };
    let Some(entries) = entries else {
        return entry_method(object, name, arguments);
    };
    let listed: Vec<(Value, Entry)> = (members(&entries).into_iter())
        .filter_map(|value| entry_of(&value).map(|entry| (value, entry)))
        .collect();
    let matching = |pick: fn(&Entry) -> &Option<String>, id: &Value| -> Vec<(Value, Entry)> {
        let id = id.to_string();
        let found = listed
            .iter()
            .filter(|(_, entry)| pick(entry).as_deref() == Some(&id));
        found.cloned().collect()
    };
    let family: fn(&Entry) -> &Option<String> = |entry| &entry.family;
    let individual: fn(&Entry) -> &Option<String> = |entry| &entry.individual;
    let ids = |found: Vec<(Value, Entry)>, pick: fn(&Entry) -> &Option<String>| {
        Value::set(found.iter().map(|(_, entry)| text_value(pick(entry))))
    };
    Ok(Some(match (name, arguments.as_slice()) {
        ("importPed", [path]) => {
            import(object, None, path, context)?;
            Value::Null
        }
        ("importPed", [options @ Value::Map(_), path]) => {
            import(object, Some(options), path, context)?;
            Value::Null
        }
        ("getEntries", []) => entries,
        ("setEntries", [items]) => {
            let replaced = match items {
                Value::Set(_) => items.clone(),
                other => Value::set(members(other)),
            };
            if let Some(Ped::Pedigree(entries)) = native::<Ped>(object).as_deref_mut() {
                *entries = replaced;
            }
            Value::Null
        }
        ("addEntry", [entry]) => {
            add(&entries, [entry.clone()]);
            Value::Null
        }
        ("addEntries", [items]) => {
            add(&entries, members(items));
            Value::Null
        }
        ("getFamilies", []) => ids(listed.clone(), family),
        ("getIndividuals", []) => ids(listed.clone(), individual),
        ("getEntriesByFamily", [id]) => {
            Value::set(matching(family, id).into_iter().map(|(value, _)| value))
        }
        ("getEntriesByIndividual", [id]) => {
            Value::set(matching(individual, id).into_iter().map(|(value, _)| value))
        }
        ("getFamiliesFromIndividual", [id]) => ids(matching(individual, id), family),
        ("getIndividualsFromFamily", [id]) => ids(matching(family, id), individual),
        ("writePed", arguments) => write(&listed, arguments, context)?,
        _ => return Ok(None),
    }))
}

fn entry_method(object: &Value, name: &str, arguments: Vec<Value>) -> Result<Option<Value>, Error> {
    let Some(mut state) = native::<Ped>(object) else {
        return Ok(None);
    };
    let Ped::Entry(entry) = &mut *state else {
        return Ok(None);
    };
    let text = |value: &Value| match value {
        Value::Null => None,
        other => id(&other.to_string()),
    };
    Ok(Some(match (name, arguments.as_slice()) {
        ("getFamily", []) => text_value(&entry.family),
        ("getIndividual", []) => text_value(&entry.individual),
        ("getFather", []) => text_value(&entry.father),
        ("getMother", []) => text_value(&entry.mother),
        ("getSex", []) => Value::string(entry.sex),
        ("getPhenotype", []) => Value::string(entry.phenotype),
        ("getAdditionalFields", []) => Value::list(
            entry
                .additional
                .iter()
                .map(|f| Value::string(f.as_str()))
                .collect(),
        ),
        ("setFamily", [value]) => {
            entry.family = text(value);
            Value::Null
        }
        ("setIndividual", [value]) => {
            entry.individual = text(value);
            Value::Null
        }
        ("setFather", [value]) => {
            entry.father = text(value);
            Value::Null
        }
        ("setMother", [value]) => {
            entry.mother = text(value);
            Value::Null
        }
        ("setSex", [value]) => {
            entry.sex = code(&value.to_string());
            Value::Null
        }
        ("setPhenotype", [value]) => {
            entry.phenotype = code(&value.to_string());
            Value::Null
        }
        ("setAdditionalFields", [value]) => {
            entry.additional = members(value).iter().map(Value::to_string).collect();
            Value::Null
        }
        ("toString", []) => Value::string(entry.text()),
        _ => return Ok(None),
    }))
}

/// The items of a list or set, in order.
fn members(value: &Value) -> Vec<Value> {
    match value {
        Value::Set(set) => set.borrow().items.iter().map(|key| key.0.clone()).collect(),
        Value::List(list) => list.borrow().items.clone(),
        _ => Vec::new(),
    }
}

fn entry_of(value: &Value) -> Option<Entry> {
    match native::<Ped>(value).as_deref() {
        Some(Ped::Entry(entry)) => Some(entry.clone()),
        _ => None,
    }
}

fn add(entries: &Value, items: impl IntoIterator<Item = Value>) {
    if let Value::Set(set) = entries {
        let mut set = set.borrow_mut();
        for item in items {
            set.items.insert(nextflow_evaluator::value::Key(item));
        }
    }
}

/// Reads a PED file into the pedigree: tab separated by default, `#` lines skipped, entries
/// whose family and individual are known already skipped, or replaced with `overwrite`.
fn import(
    pedigree: &Value,
    options: Option<&Value>,
    path: &Value,
    context: &mut Context,
) -> Result<(), Error> {
    let option = |name: &str| match options {
        Some(Value::Map(map)) => {
            let key = nextflow_evaluator::value::Key(Value::string(name));
            map.borrow().entries.get(&key).cloned()
        }
        _ => None,
    };
    let separator = option("sep").map_or_else(|| "\t".to_owned(), |sep| sep.to_string());
    let overwrite = option("overwrite").is_some_and(|value| value.truth());
    let Value::Path(file) = path else {
        return Err(no_signature("importPed", std::slice::from_ref(path)));
    };
    let separator = Pattern::new(&separator).map_err(thrown)?;
    let uri = match &file.prefix {
        Some(_) => file.uri(),
        None => format!("file://{}", file.path),
    };
    let entries = match native::<Ped>(pedigree).as_deref() {
        Some(Ped::Pedigree(entries)) => entries.clone(),
        _ => return Ok(()),
    };
    let bytes = context.host.read(file)?;
    let text = String::from_utf8_lossy(&bytes);
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.starts_with('#') {
            continue;
        }
        let fields = separator.split(line, 0).map_err(thrown)?;
        if fields.len() < 6 {
            return Err(abort(format!(
                "Could not determine PED entry at line {number} in '{uri}': expected at least6 \
                 columns, found {}",
                fields.len()
            )));
        }
        let entry = Entry {
            family: id(&fields[0]),
            individual: id(&fields[1]),
            father: id(&fields[2]),
            mother: id(&fields[3]),
            sex: code(&fields[4]),
            phenotype: code(&fields[5]),
            additional: fields[6..].to_vec(),
        };
        if entry.family.is_none() {
            let message = format!("Expected a family ID at line {number} in PED file '{uri}'");
            return Err(abort(message));
        }
        if entry.individual.is_none() {
            let message = format!("Expected an individual ID at line {number} in PED file '{uri}'");
            return Err(abort(message));
        }
        let same = |value: &Value| {
            entry_of(value).is_some_and(|known| {
                known.family == entry.family && known.individual == entry.individual
            })
        };
        let known: Vec<Value> = members(&entries).into_iter().filter(same).collect();
        if !known.is_empty() {
            if !overwrite {
                continue;
            }
            if let Value::Set(set) = &entries {
                let mut set = set.borrow_mut();
                set.items.retain(|item| !same(&item.0));
            }
        }
        add(&entries, [entry_value(entry)]);
    }
    Ok(())
}

/// `writePed([options,] [path])`: the entries of the chosen families, one line each, into the
/// file, by default `generated_peds/ped_<md5>.ped` of the run. `null` when there are none.
fn write(
    listed: &[(Value, Entry)],
    arguments: &[Value],
    context: &mut Context,
) -> Result<Value, Error> {
    let (options, path) = match arguments {
        [] => (None, None),
        [options @ Value::Map(_)] => (Some(options), None),
        [options @ Value::Map(_), path] => (Some(options), Some(path)),
        [path] => (None, Some(path)),
        _ => return Ok(Value::Null),
    };
    let option = |name: &str| match options {
        Some(Value::Map(map)) => {
            let key = nextflow_evaluator::value::Key(Value::string(name));
            map.borrow().entries.get(&key).cloned()
        }
        _ => None,
    };
    let families = option("families")
        .map(|families| members(&families))
        .unwrap_or_default();
    let overwrite = option("overwrite").is_some_and(|value| value.truth());
    let mut chosen: Vec<&Entry> = Vec::new();
    if families.is_empty() {
        chosen.extend(listed.iter().map(|(_, entry)| entry));
    }
    for family in families.iter().map(Value::to_string) {
        let found = listed
            .iter()
            .filter(|(_, entry)| entry.family.as_deref() == Some(&family));
        let before = chosen.len();
        chosen.extend(found.map(|(_, entry)| entry));
        if chosen.len() == before {
            tracing::warn!(target: "nvnieuwk.ped.Ped", "Family ID '{family}' not found in pedigree, skipping...");
        }
    }
    if chosen.is_empty() {
        tracing::warn!(target: "nvnieuwk.ped.Ped", "No PED entries to publish, skipping writing PED file");
        return Ok(Value::Null);
    }
    let lines: Vec<String> = chosen.iter().map(|entry| entry.text()).collect();
    let file = match path {
        Some(path) => PathValue::parse(&path.to_string(), &context.host.launch_dir()),
        None => {
            // The hex of the digest as a number, which drops leading zero digits.
            let digest = md5::Md5::digest(lines.concat().as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            let name = format!("generated_peds/ped_{}.ped", hex.trim_start_matches('0'));
            context.host.operator_directory()?.resolve(&name)
        }
    };
    if overwrite || context.host.metadata(&file)?.is_none() {
        let content: String = lines.iter().map(|line| format!("{line}\n")).collect();
        context.host.write(&file, content.as_bytes())?;
    }
    Ok(Value::Path(Rc::new(file)))
}

/// An error that Nextflow reports with its message only.
fn abort(message: String) -> Error {
    thrown(Exception::new("AbortOperationException", message))
}

fn thrown(exception: Exception) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span: Default::default(),
    }
}
