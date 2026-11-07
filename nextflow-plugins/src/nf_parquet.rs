//! nf-parquet 0.2.3: `splitParquet` and `countParquet` read Parquet rows as maps; uwe has no
//! Java records, so a record class fails as one that is not a record does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::number::{BigDecimal, Number};
use nextflow_evaluator::value::{Object, PathValue, State as ObjectState, Time, Value};
use parquet::basic::{ConvertedType, LogicalType, TimeUnit, Type as Physical};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use parquet::schema::types::Type;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["splitParquet", "countParquet", "toParquet"],
    call: Some(call),
    script: Some(include_str!("nf_parquet.nf")),
    ..Plugin::new("nf-parquet", Kind::Functions, State::Partial)
};

/// The options of the splitter, as Nextflow's splitters name them.
const OPTIONS: [&str; 8] = [
    "each",
    "by",
    "into",
    "autoClose",
    "limit",
    "elem",
    "record",
    "file",
];

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    match (name, arguments.as_slice()) {
        ("parquetCheck", [options]) => check(options).map(|()| Value::Null),
        ("parquetItems", [item, options]) => items(item, options, context).map(Value::list),
        ("parquetCount", [item, options]) => count(item, options, context),
        ("parquetWriter", [_, options]) => writer(options),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-parquet`"),
            span: Default::default(),
        }),
    }
}

/// Fails for an option the splitter does not know, and for the options uwe cannot follow.
fn check(options: &Value) -> Result<(), Error> {
    let entries = crate::nf_schema::entries(options);
    let record = entries
        .iter()
        .any(|(name, value)| name == "record" && value.truth());
    for (name, value) in entries {
        if !OPTIONS.contains(&name.as_str()) {
            return Err(thrown(format!(
                "Unknown argument '{name}' for operator 'split' -- Possible arguments: {}",
                OPTIONS.join(", ")
            )));
        }
        let set = !matches!(value, Value::Null | Value::Boolean(false));
        match name.as_str() {
            "record" if set && class_name(&value).is_none() => {
                return Err(thrown(format!(
                    "Value '{value}' cannot be used in in parameter 'record' for operator \
                     'split' -- Value don't match: class java.lang.Class"
                )));
            }
            "file" if set && !record => {
                return Err(thrown("A Record.class is required. Class provided null"));
            }
            "into" if set => {
                return Err(thrown(
                    "uwe does not support the `into` option of splitParquet; use the channel it \
                     returns",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `toParquet(path, [record:, by:])` checks its options as the plugin does; its record class is
/// never a Java record in uwe.
fn writer(options: &Value) -> Result<Value, Error> {
    let entries = crate::nf_schema::entries(options);
    if let Some((name, _)) = entries
        .iter()
        .find(|(name, _)| name != "record" && name != "by")
    {
        return Err(thrown(format!(
            "Unknow {name} arguments. Posible values [record, by]"
        )));
    }
    let record = entries.into_iter().find(|(name, _)| name == "record");
    let record = record.map_or(Value::Null, |(_, value)| value);
    match class_name(&record) {
        Some(class) => Err(thrown(format!("{class} must be a java Record"))),
        None => Err(thrown(format!(
            "A Record.class is required. Class provided {record}"
        ))),
    }
}

/// The Java name of a class, as the plugin names it in messages; `None` for other values.
fn class_name(value: &Value) -> Option<String> {
    match value {
        Value::Class(name) => Some(name.to_string()),
        Value::Object(object) => match &object.borrow().state {
            ObjectState::Class(class) => Some(class.def.name.replace('.', "$")),
            _ => None,
        },
        _ => None,
    }
}

/// The file of a source item, and the list item it stands in when the item is a list.
struct Source {
    list: Option<Vec<Value>>,
    index: usize,
    path: PathValue,
}

/// A whole-number option of the splitter.
fn number(options: &Value, name: &str) -> Option<i64> {
    match (crate::nf_schema::entries(options).into_iter()).find(|(key, _)| key == name) {
        Some((_, Value::Number(Number::Integer(value)))) => Some(i64::from(value)),
        Some((_, Value::Number(Number::Long(value)))) => Some(value),
        _ => None,
    }
}

/// The items that one source item gives: its rows, or lists of `by` rows, at most `limit`
/// rows per file; in a list item, the rows replace the file at `elem` or the first file.
fn items(item: &Value, options: &Value, context: &mut Context) -> Result<Vec<Value>, Error> {
    let Source { list, index, path } = source(item, options, context)?;
    let limit = number(options, "limit").and_then(|limit| usize::try_from(limit).ok());
    let mut rows = Vec::new();
    read(&path, limit, context, &mut |row| rows.push(row))?;
    let parts = match number(options, "by").and_then(|by| usize::try_from(by).ok()) {
        Some(by) if by > 0 => (rows.chunks(by))
            .map(|chunk| Value::list(chunk.to_vec()))
            .collect(),
        _ => rows,
    };
    Ok(match list {
        Some(items) => (parts.into_iter())
            .map(|part| {
                let mut copy = items.clone();
                copy[index] = part;
                Value::list(copy)
            })
            .collect(),
        None => parts,
    })
}

/// The number of items that one source item gives, read without keeping its rows.
fn count(item: &Value, options: &Value, context: &mut Context) -> Result<Value, Error> {
    let path = source(item, options, context)?.path;
    let limit = number(options, "limit").and_then(|limit| usize::try_from(limit).ok());
    let mut rows = 0usize;
    read(&path, limit, context, &mut |_| rows += 1)?;
    let parts = match number(options, "by").and_then(|by| usize::try_from(by).ok()) {
        Some(by) if by > 0 => rows.div_ceil(by),
        _ => rows,
    };
    Ok(Value::integer(i32::try_from(parts).unwrap_or(i32::MAX)))
}

/// The file to split of a source item.
fn source(item: &Value, options: &Value, context: &mut Context) -> Result<Source, Error> {
    let mut entries = crate::nf_schema::entries(options).into_iter();
    let record = entries.find(|(key, _)| key == "record");
    if let Some(class) = record.and_then(|(_, value)| class_name(&value)) {
        return Err(thrown(format!("{class} is not a Java Record")));
    }
    let (list, index) = match item {
        Value::List(list) => {
            let items = list.borrow().items.clone();
            let index = match number(options, "elem") {
                Some(elem) => usize::try_from(elem).ok(),
                None => items.iter().position(|item| matches!(item, Value::Path(_))),
            };
            let Some(index) = index.filter(|index| *index < items.len()) else {
                return Err(thrown("Cannot find a file to split in the list item"));
            };
            (Some(items), index)
        }
        _ => (None, 0),
    };
    let source = list.as_ref().map_or(item, |items| &items[index]);
    let path = match source {
        Value::Path(path) => (**path).clone(),
        Value::String(text) | Value::GString(text) => {
            PathValue::parse(text, &context.host.launch_dir())
        }
        other => {
            let simple = other.class_name();
            let class = nextflow_evaluator::value::qualified_class_name(simple).unwrap_or(simple);
            return Err(thrown(format!(
                "Object of class '{class}' does not support 'splitter' methods"
            )));
        }
    };
    Ok(Source { list, index, path })
}

/// Hands each row of a Parquet file, local or remote, to `keep` as a map from column name to
/// value, and stops after `limit` rows.
fn read(
    path: &PathValue,
    limit: Option<usize>,
    context: &mut Context,
    keep: &mut dyn FnMut(Value),
) -> Result<(), Error> {
    let bytes = context.host.read(path)?;
    let opened = SerializedFileReader::new(bytes::Bytes::from(bytes))
        .map_err(|error| thrown(format!("{} is not a Parquet file: {error}", path.uri())))?;
    let schema = opened.metadata().file_metadata().schema_descr();
    for column in schema.columns() {
        readable(&column.self_type_ptr())?;
    }
    let root = schema.root_schema().get_fields().to_vec();
    let failed = |error: parquet::errors::ParquetError| {
        thrown(format!("Error while reading Parquet file - cause: {error}"))
    };
    let rows = opened.get_row_iter(None).map_err(failed)?;
    for row in rows.take(limit.unwrap_or(usize::MAX)) {
        let row = row.map_err(failed)?;
        let entries = (row.get_column_iter().zip(&root))
            .map(|((name, field), kind)| Ok((Value::string(name.as_str()), value(field, kind)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        keep(Value::map(entries));
    }
    Ok(())
}

/// Fails for a column that the plugin cannot read, or that Parquet's row reader does not know.
fn readable(column: &Type) -> Result<(), Error> {
    let info = column.get_basic_info();
    let converted = info.converted_type();
    let float16 = info.logical_type_ref() == Some(&LogicalType::Float16);
    let known = match column.get_physical_type() {
        Physical::BYTE_ARRAY if matches!(converted, ConvertedType::NONE | ConvertedType::BSON) => {
            return Err(thrown("BINARY deserialization not supported"));
        }
        Physical::FIXED_LEN_BYTE_ARRAY if converted == ConvertedType::NONE && !float16 => {
            return Err(thrown("FIXED_LEN_BYTE_ARRAY deserialization not supported"));
        }
        Physical::INT32 => matches!(
            converted,
            ConvertedType::NONE
                | ConvertedType::INT_8
                | ConvertedType::INT_16
                | ConvertedType::INT_32
                | ConvertedType::UINT_8
                | ConvertedType::UINT_16
                | ConvertedType::UINT_32
                | ConvertedType::DATE
                | ConvertedType::TIME_MILLIS
                | ConvertedType::DECIMAL
        ),
        Physical::INT64 => matches!(
            converted,
            ConvertedType::NONE
                | ConvertedType::INT_64
                | ConvertedType::UINT_64
                | ConvertedType::TIME_MICROS
                | ConvertedType::TIMESTAMP_MILLIS
                | ConvertedType::TIMESTAMP_MICROS
                | ConvertedType::DECIMAL
        ),
        Physical::BYTE_ARRAY => matches!(
            converted,
            ConvertedType::UTF8
                | ConvertedType::ENUM
                | ConvertedType::JSON
                | ConvertedType::DECIMAL
        ),
        Physical::FIXED_LEN_BYTE_ARRAY => converted == ConvertedType::DECIMAL || float16,
        _ => true,
    };
    match known {
        true => Ok(()),
        false => Err(thrown(format!(
            "uwe cannot read the Parquet column {} of type {} {converted}",
            column.name(),
            column.get_physical_type()
        ))),
    }
}

/// The value of a field as the plugin gives it, with the column's type for its timestamps.
fn value(field: &Field, kind: &Type) -> Result<Value, Error> {
    let integer = |value: i32| Value::Number(Number::Integer(value));
    let long = |value: i64| Value::Number(Number::Long(value));
    let timestamp = match kind.is_primitive() {
        true => match kind.get_basic_info().logical_type_ref() {
            Some(LogicalType::Timestamp(stamp)) => Some((stamp.is_adjusted_to_u_t_c, stamp.unit)),
            _ => None,
        },
        false => None,
    };
    Ok(match field {
        Field::Null => Value::Null,
        Field::Bool(value) => Value::Boolean(*value),
        Field::Byte(value) => integer(i32::from(*value)),
        Field::Short(value) => integer(i32::from(*value)),
        Field::Int(value) => integer(*value),
        // Java has no unsigned types: the bits as the signed type of the same width.
        Field::UByte(value) => integer(i32::from(*value as i8)),
        Field::UShort(value) => integer(i32::from(*value as i16)),
        Field::UInt(value) => integer(*value as i32),
        Field::ULong(value) => long(*value as i64),
        Field::Long(value) | Field::TimestampMillis(value) | Field::TimestampMicros(value)
            if timestamp.is_some() =>
        {
            let (utc, unit) = timestamp.unwrap_or((false, TimeUnit::MILLIS));
            let nanos = match (field, unit) {
                (Field::TimestampMillis(_), _) | (_, TimeUnit::MILLIS) => {
                    i128::from(*value) * 1_000_000
                }
                (Field::TimestampMicros(_), _) | (_, TimeUnit::MICROS) => {
                    i128::from(*value) * 1_000
                }
                _ => i128::from(*value),
            };
            let instant = jiff::Timestamp::from_nanosecond(nanos)
                .map_err(|error| thrown(format!("a Parquet timestamp: {error}")))?;
            match utc {
                true => time(Time::Instant(instant)),
                false => time(Time::DateTime(
                    instant.to_zoned(jiff::tz::TimeZone::UTC).datetime(),
                )),
            }
        }
        Field::Long(value) => long(*value),
        Field::TimestampMillis(value) | Field::TimestampMicros(value) => long(*value),
        Field::Float16(value) => Value::Number(Number::Float(f32::from(*value))),
        Field::Float(value) => Value::Number(Number::Float(*value)),
        Field::Double(value) => Value::Number(Number::Double(*value)),
        Field::Decimal(decimal) => decimal_value(decimal.data(), decimal.scale())?,
        Field::Str(text) => Value::string(text.as_str()),
        Field::Bytes(_) => return Err(thrown("BINARY deserialization not supported")),
        Field::Date(days) => {
            let epoch = jiff::civil::date(1970, 1, 1);
            let day = epoch
                .checked_add(jiff::Span::new().days(i64::from(*days)))
                .map_err(|error| thrown(format!("a Parquet date: {error}")))?;
            time(Time::Date(day))
        }
        Field::TimeMillis(millis) => local_time(i64::from(*millis) * 1_000_000),
        Field::TimeMicros(micros) => local_time(*micros * 1_000),
        Field::Group(row) => {
            let fields = kind.get_fields();
            let entries = (row.get_column_iter().zip(fields))
                .map(|((name, field), kind)| {
                    Ok((Value::string(name.as_str()), value(field, kind)?))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Value::map(entries)
        }
        Field::ListInternal(list) => {
            let element = element(kind);
            let items = (list.elements().iter())
                .map(|item| value(item, element))
                .collect::<Result<Vec<_>, Error>>()?;
            Value::list(items)
        }
        Field::MapInternal(map) => {
            let (key_kind, value_kind) = entry_kinds(kind);
            let entries = (map.entries().iter())
                .map(|(key, item)| Ok((value(key, key_kind)?, value(item, value_kind)?)))
                .collect::<Result<Vec<_>, Error>>()?;
            Value::map(entries)
        }
    })
}

/// The type of the elements of a list column: the inner field of its repeated group, or the
/// repeated field itself in the old two-level layout.
fn element(kind: &Type) -> &Type {
    let Some(repeated) = kind.get_fields().first() else {
        return kind;
    };
    match repeated.is_group() && repeated.get_fields().len() == 1 {
        true => &repeated.get_fields()[0],
        false => repeated,
    }
}

/// The types of the keys and values of a map column.
fn entry_kinds(kind: &Type) -> (&Type, &Type) {
    let pair = kind.get_fields().first().map(|pair| pair.get_fields());
    match pair {
        Some([key, item, ..]) => (key, item),
        _ => (kind, kind),
    }
}

/// A decimal of its unscaled big-endian two's complement bytes and its scale.
fn decimal_value(bytes: &[u8], scale: i32) -> Result<Value, Error> {
    if bytes.len() > 16 {
        return Err(thrown("uwe reads Parquet decimals of at most 38 digits"));
    }
    let fill = if bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
        0xff
    } else {
        0
    };
    let mut wide = [fill; 16];
    wide[16 - bytes.len()..].copy_from_slice(bytes);
    let unscaled = i128::from_be_bytes(wide);
    let digits = unscaled.unsigned_abs().to_string();
    let scale = usize::try_from(scale).unwrap_or(0);
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    let sign = if unscaled < 0 { "-" } else { "" };
    let text = match fraction.is_empty() {
        true => format!("{sign}{whole}"),
        false => format!("{sign}{whole}.{fraction}"),
    };
    match BigDecimal::parse(&text) {
        Some(decimal) => Ok(Value::Number(Number::BigDecimal(decimal))),
        None => Err(thrown(format!("the Parquet decimal {text}"))),
    }
}

/// A `java.time.LocalTime` of nanoseconds since midnight, printed as Java prints it.
fn local_time(nanos: i64) -> Value {
    let (seconds, nanos) = (nanos / 1_000_000_000, nanos % 1_000_000_000);
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let mut text = format!("{hours:02}:{minutes:02}");
    if seconds != 0 || nanos != 0 {
        text.push_str(&format!(":{seconds:02}"));
    }
    match nanos {
        0 => {}
        nanos if nanos % 1_000_000 == 0 => text.push_str(&format!(".{:03}", nanos / 1_000_000)),
        nanos if nanos % 1_000 == 0 => text.push_str(&format!(".{:06}", nanos / 1_000)),
        nanos => text.push_str(&format!(".{nanos:09}")),
    }
    object("java.time.LocalTime", ObjectState::Text(text.into()))
}

fn time(time: Time) -> Value {
    let class = match time {
        Time::Date(_) => "java.time.LocalDate",
        Time::DateTime(_) => "java.time.LocalDateTime",
        _ => "java.time.Instant",
    };
    object(class, ObjectState::Time(time))
}

fn object(class: &'static str, state: ObjectState) -> Value {
    Value::Object(Rc::new(RefCell::new(Object { class, state })))
}

fn thrown(message: impl Into<String>) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::illegal_argument(message)),
        span: Default::default(),
    }
}
