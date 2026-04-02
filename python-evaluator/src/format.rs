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

fn format_float(number: f64, spec: &Spec) -> Result<String, Error> {
    let negative = number.is_sign_negative() && !number.is_nan();
    let magnitude = number.abs();
    let upper = spec.kind.is_some_and(|kind| kind.is_ascii_uppercase());
    let body = if magnitude.is_infinite() {
        if upper { "INF" } else { "inf" }.to_owned()
    } else if magnitude.is_nan() {
        if upper { "NAN" } else { "nan" }.to_owned()
    } else {
        match spec.kind {
            Some('f' | 'F') => format!("{magnitude:.*}", spec.precision.unwrap_or(6)),
            Some('e' | 'E') => exponent_form(magnitude, spec.precision.unwrap_or(6), upper),
            Some('g' | 'G' | 'n') => general_form(
                magnitude,
                spec.precision.unwrap_or(6),
                spec.alternate,
                upper,
            ),
            Some('%') => format!("{:.*}%", spec.precision.unwrap_or(6), magnitude * 100.0),
            None => match spec.precision {
                Some(precision) => {
                    let text = general_form(magnitude, precision, spec.alternate, false);
                    if text.contains(['.', 'e', 'n', 'i']) {
                        text
                    } else {
                        format!("{text}.0")
                    }
                }
                None => float_repr(magnitude),
            },
            Some(other) => {
                return Err(Error::value_error(format!(
                    "Unknown format code '{other}' for object of type 'float'"
                )));
            }
        }
    };
    let body = match spec.grouping {
        Some(separator) => {
            let split = body.find(['.', 'e', 'E', '%']).unwrap_or(body.len());
            let (whole, rest) = body.split_at(split);
            if whole.chars().all(|c| c.is_ascii_digit()) {
                format!("{}{rest}", group(whole, separator, 3))
            } else {
                body
            }
        }
        None => body,
    };
    let text = format!("{}{body}", sign_text(negative, spec));
    Ok(pad(&text, spec, '>'))
}

/// One step after the name of a replacement field: `.attribute` or `[key]`.
#[derive(Clone, Debug)]
pub enum Accessor {
    Attribute(String),
    Index(Value),
}

/// A parsed replacement field of `str.format`.
#[derive(Clone, Debug)]
pub struct Field {
    /// The argument name or number; empty for automatic numbering.
    pub name: String,
    pub accessors: Vec<Accessor>,
    pub conversion: Option<char>,
    pub spec: String,
}

/// Splits `name.attr[key]` into the name and its accessors.
pub fn parse_field_name(text: &str) -> Result<(String, Vec<Accessor>), Error> {
    let end = text.find(['.', '[']).unwrap_or(text.len());
    let name = text[..end].to_owned();
    let mut accessors = Vec::new();
    let mut rest = &text[end..];
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            if end == 0 {
                return Err(Error::value_error("Empty attribute in format string"));
            }
            accessors.push(Accessor::Attribute(after[..end].to_owned()));
            rest = &after[end..];
        } else if let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else {
                return Err(Error::value_error("Missing ']' in format string"));
            };
            let key = &after[..end];
            let key = if !key.is_empty() && key.chars().all(|c| c.is_ascii_digit()) {
                Value::Int(key.parse().unwrap_or(0))
            } else {
                Value::str(key)
            };
            accessors.push(Accessor::Index(key));
            rest = &after[end + 1..];
        } else {
            return Err(Error::value_error(
                "Only '.' or '[' may follow ']' in format field specifier",
            ));
        }
    }
    Ok((name, accessors))
}

/// A piece of a format template.
#[derive(Clone, Debug)]
pub enum Piece {
    Text(String),
    Field(Field),
}

/// Parses a `str.format` template into text and replacement fields.
pub fn parse_template(template: &str) -> Result<Vec<Piece>, Error> {
    let chars: Vec<char> = template.chars().collect();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '{' {
            if chars.get(index + 1) == Some(&'{') {
                text.push('{');
                index += 2;
                continue;
            }
            // Find the matching close brace, allowing nested fields in the spec.
            let mut depth = 1;
            let mut end = index + 1;
            while end < chars.len() {
                match chars[end] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            if end >= chars.len() {
                return Err(Error::value_error(
                    "Single '{' encountered in format string",
                ));
            }
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            let inner: String = chars[index + 1..end].iter().collect();
            pieces.push(Piece::Field(parse_field(&inner)?));
            index = end + 1;
        } else if c == '}' {
            if chars.get(index + 1) == Some(&'}') {
                text.push('}');
                index += 2;
                continue;
            }
            return Err(Error::value_error(
                "Single '}' encountered in format string",
            ));
        } else {
            text.push(c);
            index += 1;
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

fn parse_field(inner: &str) -> Result<Field, Error> {
    // The name ends at the first `!` or `:` outside brackets.
    let mut end = inner.len();
    let mut in_brackets = false;
    for (index, c) in inner.char_indices() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            '!' | ':' if !in_brackets => {
                end = index;
                break;
            }
            _ => {}
        }
    }
    let name_text = &inner[..end];
    let mut rest = &inner[end..];
    let mut conversion = None;
    if let Some(after) = rest.strip_prefix('!') {
        let mut chars = after.chars();
        conversion = chars.next();
        rest = chars.as_str();
        if !matches!(conversion, Some('r' | 's' | 'a')) {
            return Err(Error::value_error(
                "Unknown conversion specifier in format string",
            ));
        }
        if !rest.is_empty() && !rest.starts_with(':') {
            return Err(Error::value_error(
                "expected ':' after conversion specifier",
            ));
        }
    }
    let spec = rest.strip_prefix(':').unwrap_or(rest).to_owned();
    let (name, accessors) = parse_field_name(name_text)?;
    Ok(Field {
        name,
        accessors,
        conversion,
        spec,
    })
}

/// Follows the accessors of a field from its value.
pub fn access(
    interpreter: &mut Interpreter,
    mut value: Value,
    accessors: &[Accessor],
) -> Result<Value, Error> {
    for accessor in accessors {
        value = match accessor {
            Accessor::Attribute(name) => interpreter.get_attribute(&value, name)?,
            Accessor::Index(key) => interpreter.get_item(&value, key)?,
        };
    }
    Ok(value)
}

/// Applies `!r`, `!s` or `!a` to a value.
pub fn convert(
    interpreter: &mut Interpreter,
    value: Value,
    conversion: Option<char>,
) -> Result<Value, Error> {
    Ok(match conversion {
        Some('r' | 'a') => Value::str(interpreter.repr(&value)?),
        Some('s') => Value::str(interpreter.to_str(&value)?),
        _ => value,
    })
}

/// `template.format(*args, **kwargs)`.
pub fn format_method(
    interpreter: &mut Interpreter,
    template: &str,
    args: &Args,
) -> Result<String, Error> {
    let mut next = 0;
    let mut resolve = |interpreter: &mut Interpreter, name: &str| -> Result<Value, Error> {
        if name.is_empty() {
            let index = next;
            next += 1;
            return args.positional.get(index).cloned().ok_or_else(|| {
                Error::index_error(format!(
                    "Replacement index {index} out of range for positional args tuple"
                ))
            });
        }
        if let Ok(index) = name.parse::<usize>() {
            return args.positional.get(index).cloned().ok_or_else(|| {
                Error::index_error(format!(
                    "Replacement index {index} out of range for positional args tuple"
                ))
            });
        }
        let _ = interpreter;
        args.keyword(name)
            .cloned()
            .ok_or_else(|| Error::key_error(Value::str(name)))
    };
    let mut plain = |interpreter: &mut Interpreter, value: &Value, spec: &str| {
        format_value(interpreter, value, spec)
    };
    vformat(interpreter, template, &mut resolve, &mut plain)
}

pub type Resolve<'a> = dyn FnMut(&mut Interpreter, &str) -> Result<Value, Error> + 'a;
pub type FormatField<'a> = dyn FnMut(&mut Interpreter, &Value, &str) -> Result<String, Error> + 'a;

/// The general `string.Formatter.vformat`: `resolve` gives the value of a field name and
/// `format_field` turns a converted value and its spec into text.
pub fn vformat(
    interpreter: &mut Interpreter,
    template: &str,
    resolve: &mut Resolve<'_>,
    format_field: &mut FormatField<'_>,
) -> Result<String, Error> {
    let mut output = String::new();
    for piece in parse_template(template)? {
        match piece {
            Piece::Text(text) => output.push_str(&text),
            Piece::Field(field) => {
                let value = resolve(interpreter, &field.name)?;
                let value = access(interpreter, value, &field.accessors)?;
                let value = convert(interpreter, value, field.conversion)?;
                let spec = if field.spec.contains('{') {
                    vformat(interpreter, &field.spec, resolve, format_field)?
                } else {
                    field.spec.clone()
                };
                output.push_str(&format_field(interpreter, &value, &spec)?);
            }
        }
    }
    Ok(output)
}

/// `template % values`.
pub fn percent_format(
    interpreter: &mut Interpreter,
    template: &str,
    values: &Value,
) -> Result<String, Error> {
    let (items, mapping) = match values.plain() {
        Value::Tuple(items) => (items.to_vec(), None),
        Value::Dict(_) => (vec![values.clone()], Some(values.clone())),
        other => (vec![other.clone()], None),
    };
    let chars: Vec<char> = template.chars().collect();
    let mut output = String::new();
    let mut next = 0;
    let mut index = 0;
    let take = |next: &mut usize| -> Result<Value, Error> {
        let value = items
            .get(*next)
            .cloned()
            .ok_or_else(|| Error::type_error("not enough arguments for format string"))?;
        *next += 1;
        Ok(value)
    };
    while index < chars.len() {
        let c = chars[index];
        if c != '%' {
            output.push(c);
            index += 1;
            continue;
        }
        index += 1;
        let mut key = None;
        if chars.get(index) == Some(&'(') {
            let start = index + 1;
            let mut depth = 1;
            index += 1;
            while index < chars.len() && depth > 0 {
                match chars[index] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                index += 1;
            }
            key = Some(chars[start..index - 1].iter().collect::<String>());
        }
        let mut spec = Spec::default();
        while let Some(&flag) = chars.get(index) {
            match flag {
                '-' => spec.align = Some('<'),
                '+' => spec.sign = Some('+'),
                ' ' => {
                    if spec.sign.is_none() {
                        spec.sign = Some(' ');
                    }
                }
                '#' => spec.alternate = true,
                '0' => spec.zero = true,
                _ => break,
            }
            index += 1;
        }
        if chars.get(index) == Some(&'*') {
            spec.width = take(&mut next)?.as_int().unwrap_or(0).max(0) as usize;
            index += 1;
        } else {
            let start = index;
            while chars.get(index).is_some_and(char::is_ascii_digit) {
                index += 1;
            }
            if index > start {
                spec.width = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
            }
        }
        if chars.get(index) == Some(&'.') {
            index += 1;
            if chars.get(index) == Some(&'*') {
                spec.precision = Some(take(&mut next)?.as_int().unwrap_or(0).max(0) as usize);
                index += 1;
            } else {
                let start = index;
                while chars.get(index).is_some_and(char::is_ascii_digit) {
                    index += 1;
                }
                spec.precision = Some(
                    chars[start..index]
                        .iter()
                        .collect::<String>()
                        .parse()
                        .unwrap_or(0),
                );
            }
        }
        // Length modifiers are accepted and ignored.
        while matches!(chars.get(index), Some('h' | 'l' | 'L')) {
            index += 1;
        }
        let Some(&kind) = chars.get(index) else {
            return Err(Error::value_error("incomplete format"));
        };
        index += 1;
        if kind == '%' {
            output.push('%');
            continue;
        }
        let value = match (&key, &mapping) {
            (Some(key), Some(mapping)) => {
                interpreter.get_item(mapping, &Value::str(key.as_str()))?
            }
            (Some(_), None) => return Err(Error::type_error("format requires a mapping")),
            (None, _) => take(&mut next)?,
        };
        if spec.align == Some('<') {
            spec.zero = false;
        }
        let text = match kind {
            's' | 'r' | 'a' => {
                let mut text = if kind == 's' {
                    interpreter.to_str(&value)?
                } else {
                    interpreter.repr(&value)?
                };
                if let Some(precision) = spec.precision {
                    text = text.chars().take(precision).collect();
                }
                spec.zero = false;
                pad(&text, &spec, '>')
            }
            'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                let number = match value.plain() {
                    Value::Float(number) => crate::number::float_to_int(*number)?,
                    other if crate::number::is_number(other) => other.clone(),
                    other => {
                        return Err(Error::type_error(format!(
                            "%{kind} format: a real number is required, not {}",
                            types::type_name(other)
                        )));
                    }
                };
                spec.kind = Some(if matches!(kind, 'i' | 'u') { 'd' } else { kind });
                format_int(&number, &spec)?
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let number = to_f64(&value).ok_or_else(|| {
                    Error::type_error(format!(
                        "must be real number, not {}",
                        types::type_name(&value)
                    ))
                })?;
                spec.kind = Some(kind);
                if spec.precision.is_none() {
                    spec.precision = Some(6);
                }
                format_float(number, &spec)?
            }
            'c' => {
                let text = match value.plain() {
                    Value::Str(text) => text.to_string(),
                    other => other
                        .as_int()
                        .and_then(|code| char::from_u32(code as u32))
                        .map(String::from)
                        .unwrap_or_default(),
                };
                pad(&text, &spec, '>')
            }
            other => {
                return Err(Error::value_error(format!(
                    "unsupported format character '{other}'"
                )));
            }
        };
        output.push_str(&text);
    }
    if mapping.is_none() && next < items.len() {
        return Err(Error::type_error(
            "not all arguments converted during string formatting",
        ));
    }
    Ok(output)
}
