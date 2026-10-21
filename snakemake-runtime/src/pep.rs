//! Loads peppy-compatible PEP projects, applies sample modifiers, and merges sample tables.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "modifiers.rs"]
mod modifiers;
#[path = "pep_objects.rs"]
mod objects;

use std::path::Path;

use indexmap::IndexMap;
use python_evaluator::pandas::{DataFrame, FrameData, Labels, is_missing};
use python_evaluator::{Args, Dict, Error, Interpreter, Value};

pub use objects::{Project, Sample};

/// The attributes of a sample, in the order of the table.
type Attributes = IndexMap<String, Value>;

fn peppy_error(class: &str, message: impl Into<String>) -> Error {
    Error::new(class, message.into())
}

/// Calls a function of a module, like `os.path.expandvars`.
fn call(
    interpreter: &mut Interpreter,
    module: &str,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let module = interpreter.import_module(module)?;
    let function = interpreter.get_attribute(&module, name)?;
    interpreter.call(&function, args)
}

fn method(
    interpreter: &mut Interpreter,
    object: &Value,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let function = interpreter.get_attribute(object, name)?;
    interpreter.call(&function, args)
}

fn get<'a>(mapping: &'a Attributes, key: &str) -> Option<&'a Value> {
    mapping.get(key).filter(|value| !value.is_none())
}

/// A path of the config made absolute next to the config file, after `~` and variables.
fn absolute(
    interpreter: &mut Interpreter,
    path: &str,
    config_file: &Path,
) -> Result<String, Error> {
    if Path::new(path).is_absolute() || path.contains("://") {
        return Ok(path.to_owned());
    }
    let expanded = python_evaluator::host::expand_home(path);
    let expanded = call(
        interpreter,
        "os.path",
        "expandvars",
        Args::new(vec![Value::str(expanded)]),
    )?;
    let expanded = interpreter.to_str(&expanded)?;
    if Path::new(&expanded).is_absolute() {
        return Ok(expanded);
    }
    let folder = config_file.parent().unwrap_or(Path::new(""));
    Ok(folder.join(path).to_string_lossy().into_owned())
}

/// The config of a project file as a mapping, with the configs it imports first.
fn read_config(interpreter: &mut Interpreter, file: &Path) -> Result<Attributes, Error> {
    let resolved = interpreter.host.resolve(&file.to_string_lossy());
    let text = interpreter.host.read(&resolved).map_err(|_| {
        Error::new(
            "OSError",
            format!(
                "Project config file path does not exist: {}",
                file.display()
            ),
        )
    })?;
    let loaded = python_evaluator::modules::data::load_yaml(&String::from_utf8_lossy(&text))?;
    if !matches!(loaded.plain(), Value::Dict(_)) {
        return Err(Error::new(
            "AssertionError",
            "Config file parse did not yield a Mapping",
        ));
    }
    let mut own = Attributes::new();
    for (key, value) in interpreter.mapping_items(&loaded)? {
        own.insert(interpreter.to_str(&key)?, value);
    }
    let mut config = Attributes::new();
    let imports = match get(&own, "project_modifiers") {
        Some(modifiers) => interpreter
            .get_item(modifiers, &Value::str("import"))
            .ok()
            .filter(|value| !value.is_none()),
        None => None,
    };
    if let Some(imports) = imports {
        for item in interpreter.iterate(&imports)? {
            let text = interpreter.to_str(&item)?;
            let path = absolute(interpreter, &text, file)?;
            let resolved = interpreter.host.resolve(&path);
            if interpreter.host.stat(&resolved).is_some() {
                config.extend(read_config(interpreter, Path::new(&path))?);
            }
        }
    }
    config.extend(own);
    Ok(config)
}

/// The PEP version of the config, which must be a text like `2.0.0`.
fn pep_version(config: &Attributes) -> Result<String, Error> {
    let Some(value) = get(config, "pep_version") else {
        return Ok("2.0.0".into());
    };
    let Some(text) = value.as_str() else {
        return Err(peppy_error(
            "InvalidConfigFileException",
            "pep_version must be a string",
        ));
    };
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 3 {
        return Err(peppy_error(
            "InvalidConfigFileException",
            format!("Version string is not tripartite: {text}"),
        ));
    }
    let numbers: Result<Vec<i64>, _> = parts.iter().map(|part| part.parse::<i64>()).collect();
    let Ok(numbers) = numbers else {
        return Err(peppy_error(
            "InvalidConfigFileException",
            format!("Version string elements are not coercible to integers: {text}"),
        ));
    };
    if numbers[0] < 2 {
        return Err(peppy_error(
            "InvalidConfigFileException",
            format!(
                "PEP version is invalid: {text}. Please use version 2.0.0 or previous versions of peppy package."
            ),
        ));
    }
    Ok(numbers
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// A sample or subsample table as peppy's parsers read it: texts, with missing values as
/// `""` for the sample table.
fn read_table(interpreter: &mut Interpreter, path: &str, blank: bool) -> Result<Value, Error> {
    let extension = Path::new(path)
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    let separator = match extension.as_str() {
        "csv" => ",",
        "tsv" => "\t",
        "xlsx" => return read_excel(interpreter, path, blank),
        _ => {
            return Err(peppy_error(
                "InvalidSampleTableFileException",
                format!(
                    "No parser found for extension: {extension}. Supported sample table extensions: ['csv', 'tsv', 'xlsx']"
                ),
            ));
        }
    };
    let str_class = interpreter
        .builtins
        .get("str")
        .unwrap_or_else(|| Value::str("str"));
    let mut args = Args::new(vec![Value::str(path)]);
    args.keywords.push(("sep".into(), Value::str(separator)));
    args.keywords.push(("dtype".into(), str_class));
    args.keywords.push(("index_col".into(), Value::Bool(false)));
    args.keywords
        .push(("keep_default_na".into(), Value::Bool(false)));
    args.keywords
        .push(("na_values".into(), Value::list(vec![Value::str("")])));
    let table = call(interpreter, "pandas", "read_csv", args)?;
    if !blank {
        return Ok(table);
    }
    let mut data = table
        .as_native::<DataFrame>()
        .map(DataFrame::data)
        .ok_or_else(|| Error::type_error("read_csv did not give a DataFrame"))?;
    for column in &mut data.data {
        for value in column.iter_mut() {
            if is_missing(value) {
                *value = Value::str("");
            }
        }
    }
    Ok(DataFrame::from_data(data))
}

/// The first sheet of an Excel table as pandas' `read_excel` reads it with peppy's settings:
/// texts by column, empty cells missing (or `""` with `blank`), blank rows left out.
fn read_excel(interpreter: &mut Interpreter, path: &str, blank: bool) -> Result<Value, Error> {
    use calamine::Reader;
    let resolved = interpreter.host.resolve(path);
    if !resolved.is_file() {
        return Err(Error::file_not_found(path));
    }
    let unreadable = |error: String| Error::value_error(format!("{path}: {error}"));
    let mut book =
        calamine::open_workbook_auto(&resolved).map_err(|error| unreadable(error.to_string()))?;
    let sheet = (book.worksheet_range_at(0))
        .ok_or_else(|| unreadable("the workbook has no sheet".into()))?
        .map_err(|error| unreadable(error.to_string()))?;
    // Columns before the first used one are empty columns of the table.
    let skipped = sheet.start().map_or(0, |(_, column)| column as usize);
    let missing = || match blank {
        true => Value::str(""),
        false => Value::Float(f64::NAN),
    };
    let mut rows = Vec::new();
    for row in sheet.rows() {
        let mut cells = vec![None; skipped];
        for cell in row {
            cells.push(excel_text(interpreter, cell)?);
        }
        if cells.iter().any(Option::is_some) {
            rows.push(cells);
        }
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut header = if rows.is_empty() {
        Vec::new()
    } else {
        rows.remove(0)
    };
    header.resize(width, None);
    let mut columns: Vec<Value> = Vec::new();
    for (position, name) in header.into_iter().enumerate() {
        let name = name.unwrap_or_else(|| format!("Unnamed: {position}"));
        // pandas renames a repeated column name `a` to `a.1`, `a.2` and so on.
        let mut unique = name.clone();
        let mut count = 0;
        while columns
            .iter()
            .any(|column| column.as_str() == Some(&unique))
        {
            count += 1;
            unique = format!("{name}.{count}");
        }
        columns.push(Value::str(unique));
    }
    let data = (0..width)
        .map(|column| {
            (rows.iter())
                .map(|row| match row.get(column) {
                    Some(Some(text)) => Value::str(text.as_str()),
                    _ => missing(),
                })
                .collect()
        })
        .collect();
    let index = Labels::range(rows.len());
    let columns = Labels::single(columns, Value::None);
    Ok(DataFrame::from_data(FrameData::new(columns, data, index)))
}

/// The text of a cell as pandas gives it with `dtype=str`, or none for an empty one.
fn excel_text(
    interpreter: &mut Interpreter,
    cell: &calamine::Data,
) -> Result<Option<String>, Error> {
    use calamine::Data;
    Ok(match cell {
        Data::Empty => None,
        Data::String(text) if text.is_empty() => None,
        Data::String(text) | Data::DateTimeIso(text) | Data::DurationIso(text) => {
            Some(text.clone())
        }
        Data::Int(number) => Some(number.to_string()),
        // openpyxl gives whole numbers as integers.
        Data::Float(number) if number.fract() == 0.0 && number.abs() < 1e15 => {
            Some(format!("{number:.0}"))
        }
        Data::Float(number) => Some(interpreter.to_str(&Value::Float(*number))?),
        Data::Bool(value) => Some(if *value { "True" } else { "False" }.to_owned()),
        Data::Error(error) => Some(error.to_string()),
        Data::DateTime(time) => Some(excel_time(time.as_f64(), time.is_datetime())),
    })
}

/// The text of an Excel date, `2024-01-31 00:00:00`, of a time of day without a date or of a
/// duration, as Python prints the values of openpyxl. Days count from 1899-12-30.
fn excel_time(serial: f64, datetime: bool) -> String {
    let seconds = (serial * 86400.0).round() as i64;
    let (days, time) = (seconds.div_euclid(86400), seconds.rem_euclid(86400));
    let (hours, minutes, rest) = (time / 3600, time / 60 % 60, time % 60);
    if !datetime {
        let clock = format!("{hours}:{minutes:02}:{rest:02}");
        return match days {
            0 => clock,
            1 | -1 => format!("{days} day, {clock}"),
            _ => format!("{days} days, {clock}"),
        };
    }
    let clock = format!("{hours:02}:{minutes:02}:{rest:02}");
    if days == 0 {
        return clock;
    }
    // Howard Hinnant's days-to-civil, shifted from 1970-01-01 to 1899-12-30.
    let shifted = days - 25569 + 719468;
    let era = shifted.div_euclid(146097);
    let day = shifted - era * 146097;
    let year = (day - day / 1460 + day / 36524 - day / 146096) / 365;
    let day_of_year = day - (365 * year + year / 4 - year / 100);
    let month = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = year + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{date:02} {clock}")
}

/// The rows of a table as attributes by column.
fn rows(interpreter: &mut Interpreter, table: &Value) -> Result<Vec<Attributes>, Error> {
    let Some(frame) = table.as_native::<DataFrame>() else {
        return Ok(Vec::new());
    };
    let data = frame.data();
    let mut names = Vec::new();
    for label in &data.columns.values {
        names.push(interpreter.to_str(label)?);
    }
    Ok((0..data.rows())
        .map(|row| {
            names
                .iter()
                .enumerate()
                .map(|(column, name)| (name.clone(), data.data[column][row].clone()))
                .collect()
        })
        .collect())
}

/// A table of samples with the sample names as index, keeping their column.
fn indexed(interpreter: &mut Interpreter, table: Value, index: &str) -> Result<Value, Error> {
    let has_index = table
        .as_native::<DataFrame>()
        .is_some_and(|frame| frame.data().column_position(&Value::str(index)).is_some());
    if !has_index {
        return Ok(table);
    }
    let mut args = Args::new(vec![Value::str(index)]);
    args.keywords.push(("drop".into(), Value::Bool(false)));
    method(interpreter, &table, "set_index", args)
}

/// Loads the project of a `pepfile:`.
pub fn load(interpreter: &mut Interpreter, path: &str) -> Result<Value, Error> {
    let file = Path::new(path);
    let mut config = read_config(interpreter, file)?;
    let index = match get(&config, "sample_table_index") {
        Some(value) => interpreter.to_str(value)?,
        None => "sample_name".to_owned(),
    };
    let version = pep_version(&config)?;
    config.insert("pep_version".into(), Value::str(version));
    for key in ["sample_table", "subsample_table"] {
        let Some(value) = get(&config, key).cloned() else {
            continue;
        };
        let made = match value.plain() {
            Value::List(_) => {
                let mut paths = Vec::new();
                for item in interpreter.iterate(&value)? {
                    let text = interpreter.to_str(&item)?;
                    paths.push(Value::str(absolute(interpreter, &text, file)?));
                }
                Value::list(paths)
            }
            _ => {
                let text = interpreter.to_str(&value)?;
                Value::str(absolute(interpreter, &text, file)?)
            }
        };
        config.insert(key.into(), made);
    }
    let table = match get(&config, "sample_table").and_then(Value::as_str) {
        Some(path) => Some(read_table(interpreter, path, true)?),
        None => None,
    };
    let mut subsample_tables = Vec::new();
    if let Some(value) = get(&config, "subsample_table").cloned() {
        let paths = match value.plain() {
            Value::List(_) => interpreter.iterate(&value)?,
            _ => vec![value],
        };
        for path in paths {
            let path = interpreter.to_str(&path)?;
            subsample_tables.push(read_table(interpreter, &path, false)?);
        }
    }
    let mut samples = match &table {
        Some(table) => rows(interpreter, table)?,
        None => Vec::new(),
    };
    let modifiers = get(&config, "sample_modifiers").cloned();
    modifiers::modify(
        interpreter,
        &mut samples,
        modifiers.as_ref(),
        &index,
        &subsample_tables,
    )?;
    // Without sample modifiers, peppy keeps the table it read.
    let sample_table = match (&table, &modifiers) {
        (Some(table), None) => table.clone(),
        _ => {
            let records: Vec<Value> = samples
                .iter()
                .map(|sample| {
                    Value::dict_of(
                        sample
                            .iter()
                            .filter(|(key, _)| !key.starts_with('_'))
                            .map(|(key, value)| (key.clone(), value.clone())),
                    )
                })
                .collect();
            call(
                interpreter,
                "pandas",
                "DataFrame",
                Args::new(vec![Value::list(records)]),
            )?
        }
    };
    let sample_table = indexed(interpreter, sample_table, &index)?;
    let subsample_table = match subsample_tables.len() {
        0 => Value::None,
        1 => subsample_tables[0].clone(),
        _ => Value::list(subsample_tables),
    };
    let mut config_dict = Dict::default();
    for (key, value) in &config {
        config_dict.insert(key, value.clone());
    }
    Ok(Value::native(Project {
        config: Value::dict(config_dict),
        config_file: path.to_owned(),
        index,
        sample_table,
        subsample_table,
        samples: samples
            .into_iter()
            .map(|attributes| Value::native(Sample(attributes)))
            .collect(),
    }))
}
