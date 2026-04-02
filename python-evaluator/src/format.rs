//! Text of values: `repr`, the format specification mini-language, `str.format` and `%`
//! formatting, with Python's output.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::number::{float_repr, to_f64};
use crate::types;
use crate::value::{Args, Value};

/// A text in quotes as `repr()` shows it: single quotes unless the text holds single quotes
/// and no double quotes.
pub fn quote(text: &str) -> String {
    let delimiter = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push(delimiter);
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c == delimiter => {
                quoted.push('\\');
                quoted.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                quoted.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => quoted.push(c),
        }
    }
    quoted.push(delimiter);
    quoted
}

/// `repr()` of bytes.
pub fn bytes_repr(bytes: &[u8]) -> String {
    let mut text = String::from("b'");
    for byte in bytes {
        match byte {
            b'\\' => text.push_str("\\\\"),
            b'\'' => text.push_str("\\'"),
            b'\n' => text.push_str("\\n"),
            b'\r' => text.push_str("\\r"),
            b'\t' => text.push_str("\\t"),
            0x20..=0x7e => text.push(*byte as char),
            other => text.push_str(&format!("\\x{other:02x}")),
        }
    }
    text.push('\'');
    text
}

/// `repr()` without calling Python code: objects of Python classes show their class only.
pub fn plain_repr(value: &Value) -> String {
    match value.plain() {
        Value::None => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Int(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Float(value) => float_repr(*value),
        Value::Str(text) => quote(text),
        Value::Bytes(bytes) => bytes_repr(bytes),
        Value::List(items) => {
            let items: Vec<String> = items.borrow().iter().map(plain_repr).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(plain_repr).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        Value::Dict(dict) => {
            let parts: Vec<String> = dict
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| format!("{}: {}", plain_repr(&key.0), plain_repr(value)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Set(items) => {
            let items = items.borrow();
            if items.is_empty() {
                return "set()".into();
            }
            let parts: Vec<String> = items.iter().map(|key| plain_repr(&key.0)).collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::FrozenSet(items) => {
            let parts: Vec<String> = items.iter().map(|key| plain_repr(&key.0)).collect();
            format!("frozenset({{{}}})", parts.join(", "))
        }
        Value::Range(range) => {
            if range.step == 1 {
                format!("range({}, {})", range.start, range.stop)
            } else {
                format!("range({}, {}, {})", range.start, range.stop, range.step)
            }
        }
        Value::Slice(parts) => format!(
            "slice({}, {}, {})",
            plain_repr(&parts[0]),
            plain_repr(&parts[1]),
            plain_repr(&parts[2])
        ),
        Value::Function(function) => format!("<function {}>", function.qualname),
        Value::Builtin(builtin) => format!("<built-in function {}>", builtin.name),
        Value::Method(_) => "<bound method>".into(),
        Value::Class(class) => {
            if &*class.module == "builtins" {
                format!("<class '{}'>", class.name)
            } else {
                format!("<class '{}.{}'>", class.module, class.name)
            }
        }
        Value::Instance(instance) => {
            if instance.class.builtin == Some(types::TypeTag::BaseException) {
                let args = crate::error::exception_args(value);
                let parts: Vec<String> = args.iter().map(plain_repr).collect();
                format!("{}({})", instance.class.name, parts.join(", "))
            } else {
                format!("<{} object>", instance.class.name)
            }
        }
        Value::Module(module) => format!("<module '{}'>", module.name),
        Value::Iterator(_) => "<iterator object>".into(),
        Value::Native(object) => format!("<{} object>", object.type_name()),
        Value::Ellipsis => "Ellipsis".into(),
        Value::NotImplemented => "NotImplemented".into(),
        Value::Tagged(_) => unreachable!("plain values are never tagged"),
    }
}

/// `str()` without calling Python code.
pub fn plain_str(value: &Value) -> String {
    match value.plain() {
        Value::Str(text) => text.to_string(),
        Value::Instance(instance)
            if instance.class.builtin == Some(types::TypeTag::BaseException) =>
        {
            crate::error::exception_text(value)
        }
        other => plain_repr(other),
    }
}

pub fn debug_repr(value: &Value) -> String {
    match value {
        Value::Tagged(tagged) => {
            let tags: Vec<&str> = tagged.tags.keys().map(|key| &**key).collect();
            format!("{}[{}]", plain_repr(&tagged.value), tags.join(","))
        }
        other => plain_repr(other),
    }
}

/// A parsed format specification: `[[fill]align][sign][z][#][0][width][grouping][.precision][type]`.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub fill: Option<char>,
    pub align: Option<char>,
    pub sign: Option<char>,
    pub alternate: bool,
    pub zero: bool,
    pub width: usize,
    pub grouping: Option<char>,
    pub precision: Option<usize>,
    pub kind: Option<char>,
}

pub fn parse_spec(text: &str) -> Result<Spec, Error> {
    let chars: Vec<char> = text.chars().collect();
    let mut spec = Spec::default();
    let mut index = 0;
    let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
    if chars.len() >= 2 && is_align(chars[1]) {
        spec.fill = Some(chars[0]);
        spec.align = Some(chars[1]);
        index = 2;
    } else if !chars.is_empty() && is_align(chars[0]) {
        spec.align = Some(chars[0]);
        index = 1;
    }
    if let Some(&c) = chars.get(index)
        && matches!(c, '+' | '-' | ' ')
    {
        spec.sign = Some(c);
        index += 1;
    }
    if chars.get(index) == Some(&'z') {
        index += 1;
    }
    if chars.get(index) == Some(&'#') {
        spec.alternate = true;
        index += 1;
    }
    if chars.get(index) == Some(&'0') {
        spec.zero = true;
        index += 1;
    }
    let start = index;
    while chars.get(index).is_some_and(char::is_ascii_digit) {
        index += 1;
    }
    if index > start {
        let digits: String = chars[start..index].iter().collect();
        spec.width = digits.parse().unwrap_or(0);
    }
    if let Some(&c) = chars.get(index)
        && matches!(c, ',' | '_')
    {
        spec.grouping = Some(c);
        index += 1;
    }
    if chars.get(index) == Some(&'.') {
        index += 1;
        let start = index;
        while chars.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return Err(Error::value_error("Format specifier missing precision"));
        }
        let digits: String = chars[start..index].iter().collect();
        spec.precision = digits.parse().ok();
    }
    if let Some(&c) = chars.get(index) {
        spec.kind = Some(c);
        index += 1;
    }
    if index != chars.len() {
        return Err(Error::value_error("Invalid format specifier"));
    }
    Ok(spec)
}

/// `format(value, spec)`.
pub fn format_value(
    interpreter: &mut Interpreter,
    value: &Value,
    spec: &str,
) -> Result<String, Error> {
    if spec.is_empty() {
        return interpreter.to_str(value);
    }
    match value.plain() {
        Value::Bool(_) | Value::Int(_) | Value::BigInt(_) => {
            let parsed = parse_spec(spec)?;
            format_int(value, &parsed)
        }
        Value::Float(number) => {
            let parsed = parse_spec(spec)?;
            format_float(*number, &parsed)
        }
        Value::Str(text) => {
            let parsed = parse_spec(spec)?;
            if parsed.kind.is_some_and(|kind| kind != 's') {
                return Err(Error::value_error(format!(
                    "Unknown format code '{}' for object of type 'str'",
                    parsed.kind.unwrap_or_default()
                )));
            }
            let text = match parsed.precision {
                Some(precision) => text.chars().take(precision).collect(),
                None => text.to_string(),
            };
            Ok(pad(&text, &parsed, '<'))
        }
        Value::Instance(_) => {
            if let Some(method) = interpreter.lookup_special(value, "__format__") {
                let result = interpreter.call(&method, Args::new(vec![Value::str(spec)]))?;
                return interpreter.to_str(&result);
            }
            let text = interpreter.to_str(value)?;
            format_value(interpreter, &Value::str(text), spec)
        }
        Value::None | Value::List(_) | Value::Tuple(_) | Value::Dict(_) => {
            Err(Error::type_error(format!(
                "unsupported format string passed to {}.__format__",
                types::type_name(value)
            )))
        }
        _ => {
            let text = interpreter.to_str(value)?;
            format_value(interpreter, &Value::str(text), spec)
        }
    }
}

/// Pads a text to the width of a spec, with the default alignment given.
fn pad(text: &str, spec: &Spec, default: char) -> String {
    let length = text.chars().count();
    if length >= spec.width {
        return text.to_owned();
    }
    let fill = spec.fill.unwrap_or(if spec.zero { '0' } else { ' ' });
    let align = spec.align.unwrap_or(if spec.zero { '=' } else { default });
    let missing = spec.width - length;
    let filler = |count: usize| std::iter::repeat_n(fill, count).collect::<String>();
    match align {
        '<' => format!("{text}{}", filler(missing)),
        '^' => format!(
            "{}{text}{}",
            filler(missing / 2),
            filler(missing - missing / 2)
        ),
        '=' => {
            // Padding goes after the sign and prefix.
            let split = text
                .char_indices()
                .find(|(_, c)| !matches!(c, '+' | '-' | ' '))
                .map_or(text.len(), |(index, _)| index);
            let (sign, rest) = text.split_at(split);
            let (prefix, rest) =
                if rest.len() >= 2 && matches!(&rest[..2], "0x" | "0X" | "0o" | "0b") {
                    rest.split_at(2)
                } else {
                    ("", rest)
                };
            format!("{sign}{prefix}{}{rest}", filler(missing))
        }
        _ => format!("{}{text}", filler(missing)),
    }
}

fn group(digits: &str, separator: char, size: usize) -> String {
    let mut grouped = String::new();
    let count = digits.len();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (count - index).is_multiple_of(size) {
            grouped.push(separator);
        }
        grouped.push(digit);
    }
    grouped
}

fn sign_text(negative: bool, spec: &Spec) -> &'static str {
    match (negative, spec.sign) {
        (true, _) => "-",
        (false, Some('+')) => "+",
        (false, Some(' ')) => " ",
        _ => "",
    }
}

fn format_int(value: &Value, spec: &Spec) -> Result<String, Error> {
    let kind = spec.kind.unwrap_or('d');
    if matches!(kind, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%') {
        let number = to_f64(value).unwrap_or_default();
        return format_float(number, spec);
    }
    let number = crate::number::to_bigint(value).unwrap_or_default();
    let negative = number < num_bigint::BigInt::from(0);
    let magnitude = if negative { -number } else { number };
    let (digits, prefix) = match kind {
        'd' | 'n' => (magnitude.to_string(), ""),
        'x' => (magnitude.to_str_radix(16), "0x"),
        'X' => (magnitude.to_str_radix(16).to_uppercase(), "0X"),
        'o' => (magnitude.to_str_radix(8), "0o"),
        'b' => (magnitude.to_str_radix(2), "0b"),
        'c' => {
            let code = num_traits::ToPrimitive::to_u32(&magnitude).unwrap_or_default();
            let text = char::from_u32(code).map(String::from).unwrap_or_default();
            return Ok(pad(&text, spec, '<'));
        }
        's' => {
            return Err(Error::value_error(
                "Unknown format code 's' for object of type 'int'",
            ));
        }
        other => {
            return Err(Error::value_error(format!(
                "Unknown format code '{other}' for object of type 'int'"
            )));
        }
    };
    let digits = match spec.grouping {
        Some(separator) => group(
            &digits,
            separator,
            if kind == 'd' || kind == 'n' { 3 } else { 4 },
        ),
        None => digits,
    };
    let prefix = if spec.alternate { prefix } else { "" };
    let text = format!("{}{prefix}{digits}", sign_text(negative, spec));
    Ok(pad(&text, spec, '>'))
}

/// Python's `%e` style: at least two exponent digits and a sign.
fn exponent_form(number: f64, precision: usize, upper: bool) -> String {
    let text = format!("{number:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').expect("an exponent");
    let exponent: i32 = exponent.parse().expect("a number");
    let sign = if exponent < 0 { '-' } else { '+' };
    let text = format!("{mantissa}e{sign}{:02}", exponent.abs());
    if upper { text.to_uppercase() } else { text }
}

fn general_form(number: f64, precision: usize, alternate: bool, upper: bool) -> String {
    let precision = precision.max(1);
    if number == 0.0 {
        let text = if alternate {
            format!("{:.*}", precision - 1, 0.0)
        } else {
            "0".into()
        };
        return if number.is_sign_negative() {
            format!("-{text}")
        } else {
            text
        };
    }
    // The exponent after rounding to the precision.
    let rounded = format!("{:.*e}", precision - 1, number);
    let exponent: i32 = rounded
        .split_once('e')
        .and_then(|(_, exponent)| exponent.parse().ok())
        .unwrap_or(0);
    let text = if exponent >= -4 && exponent < precision as i32 {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        format!("{number:.decimals$}")
    } else {
        exponent_form(number, precision - 1, upper)
    };
    if alternate {
        return text;
    }
    strip_zeros(&text)
}

/// Removes trailing zeros of the fraction, and the point if nothing remains after it.
fn strip_zeros(text: &str) -> String {
    let (number, exponent) = match text.find(['e', 'E']) {
        Some(index) => text.split_at(index),
        None => (text, ""),
    };
    let number = if number.contains('.') {
        number.trim_end_matches('0').trim_end_matches('.')
    } else {
        number
    };
    format!("{number}{exponent}")
}
