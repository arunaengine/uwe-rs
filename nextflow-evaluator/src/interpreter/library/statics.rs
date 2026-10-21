//! Static methods of Java classes that scripts and configs call, like `System.getenv('X')`
//! and `Math.max(a, b)`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;

use crate::number::Number;
use crate::units::java_round;
use crate::value::{JsonOptions, State, Time, Value};

use super::super::{AtSpan, Error, Interpreter};
use super::{format, json};

impl Interpreter<'_> {
    /// `Class.name(arguments)`, or `None` when the class has no such method.
    pub(in crate::interpreter) fn static_method(
        &mut self,
        class: &str,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match (class, name, arguments) {
            ("UUID", "randomUUID", []) => uuid_object(uuid::Uuid::new_v4()),
            ("UUID", "fromString", [Value::String(text) | Value::GString(text)]) => {
                uuid_object(parse_uuid(text).at(span)?)
            }
            ("UUID", "fromString", [Value::Null]) => {
                return Err(super::super::thrown(
                    crate::exception::Exception::new("NullPointerException", "UUID text is null"),
                    span,
                ));
            }
            ("Pattern", "compile", [text]) => {
                let pattern = crate::regex::Pattern::new(&text.to_string()).at(span)?;
                Value::Pattern(std::rc::Rc::new(pattern))
            }
            ("Pattern", "quote", [text]) => Value::string(format!("\\Q{text}\\E")),
            ("Pattern", "matches", [regex, text]) => {
                let pattern = crate::regex::Pattern::new(&regex.to_string()).at(span)?;
                Value::Boolean(pattern.matches(&text.to_string()))
            }
            // Numbers are formatted as the root locale does, whatever locale is given.
            ("String", "format", [Value::Object(locale), pattern, rest @ ..])
                if matches!(locale.borrow().state, State::Locale(_)) =>
            {
                Value::string(format::format(&pattern.to_string(), rest).at(span)?)
            }
            ("String", "format", [pattern, rest @ ..]) => {
                Value::string(format::format(&pattern.to_string(), rest).at(span)?)
            }
            // `java.text.NumberFormat` of a locale, and `java.time.format.DateTimeFormatter` of a
            // pattern, which formats in the local time zone whatever zone is given.
            ("NumberFormat", "getInstance" | "getNumberInstance", [] | [_]) => {
                text_object("java.text.NumberFormat", "en_US")
            }
            ("DateTimeFormatter", "ofPattern", [pattern]) => {
                text_object("java.time.format.DateTimeFormatter", &pattern.to_string())
            }
            (
                "LocalDate" | "LocalDateTime" | "Instant" | "java.time.Duration" | "DayOfWeek",
                _,
                _,
            ) => match super::time::time_static(class, name, arguments) {
                Some(result) => result.at(span)?,
                None => return Ok(None),
            },
            // `java.util.Collections`: sorting in place, extremes, views and empty collections.
            ("Collections", "sort", [list @ Value::List(_), rest @ ..]) if rest.len() <= 1 => {
                self.call_method(list, "sort", rest.to_vec(), span)?;
                Value::Null
            }
            ("Collections", "reverse", [Value::List(list)]) => {
                list.borrow_mut().items.reverse();
                Value::Null
            }
            ("Collections", "max" | "min", [items, rest @ ..]) if rest.len() <= 1 => {
                self.call_method(items, name, rest.to_vec(), span)?
            }
            (
                "Collections",
                "unmodifiableList"
                | "unmodifiableSet"
                | "unmodifiableMap"
                | "unmodifiableCollection"
                | "synchronizedList"
                | "synchronizedMap",
                [value],
            ) => value.clone(),
            ("Collections", "emptyList", []) => Value::list(Vec::new()),
            ("Collections", "emptySet", []) => Value::set([]),
            ("Collections", "emptyMap", []) => Value::map([]),
            ("Collections", "singletonList" | "singleton", [value]) => {
                Value::list(vec![value.clone()])
            }
            // `java.util.Objects`.
            ("Objects", "equals", [left, right]) => Value::Boolean(match (left, right) {
                (Value::Null, Value::Null) => true,
                (Value::Null, _) | (_, Value::Null) => false,
                (left, right) => left.java_equals(right),
            }),
            ("Objects", "hash", values) => {
                let hash = values.iter().fold(1i32, |hash, value| {
                    let item = if matches!(value, Value::Null) {
                        0
                    } else {
                        value.java_hash()
                    };
                    hash.wrapping_mul(31).wrapping_add(item)
                });
                Value::integer(hash)
            }
            ("Objects", "hashCode", [value]) => Value::integer(if matches!(value, Value::Null) {
                0
            } else {
                value.java_hash()
            }),
            ("Objects", "isNull" | "nonNull", [value]) => {
                Value::Boolean(matches!(value, Value::Null) == (name == "isNull"))
            }
            ("Objects", "requireNonNull", [Value::Null, rest @ ..]) if rest.len() <= 1 => {
                let message = rest.first().map_or(String::new(), Value::to_string);
                let exception = crate::exception::Exception::new("NullPointerException", message);
                return Err(super::super::thrown(exception, span));
            }
            ("Objects", "requireNonNull", [value, rest @ ..]) if rest.len() <= 1 => value.clone(),
            ("Objects", "toString", [value]) => Value::string(value.to_string()),
            ("Objects", "toString", [Value::Null, default]) => default.clone(),
            ("Objects", "toString", [value, _]) => Value::string(value.to_string()),
            // `java.util.Optional`.
            ("Optional", "of", [Value::Null]) => {
                let exception = crate::exception::Exception::new("NullPointerException", "");
                return Err(super::super::thrown(exception, span));
            }
            ("Optional", "of" | "ofNullable", [value]) => super::optional(value.clone()),
            ("Optional", "empty", []) => super::optional(Value::Null),
            // `java.lang.Character` tests and case changes of one character.
            ("Character", _, [Value::Character(unit)]) => {
                let character = char::from_u32(u32::from(*unit)).unwrap_or('\u{fffd}');
                let changed = |text: String| {
                    let mut units = text.encode_utf16();
                    match (units.next(), units.next()) {
                        (Some(unit), None) => Value::Character(unit),
                        _ => Value::Character(*unit),
                    }
                };
                match name {
                    "isDigit" => Value::Boolean(character.is_ascii_digit()),
                    "isLetter" | "isAlphabetic" => Value::Boolean(character.is_alphabetic()),
                    "isLetterOrDigit" => Value::Boolean(character.is_alphanumeric()),
                    "isWhitespace" => Value::Boolean(character.is_whitespace()),
                    "isUpperCase" => Value::Boolean(character.is_uppercase()),
                    "isLowerCase" => Value::Boolean(character.is_lowercase()),
                    "toUpperCase" => changed(character.to_uppercase().collect()),
                    "toLowerCase" => changed(character.to_lowercase().collect()),
                    _ => return Ok(None),
                }
            }
            ("URLEncoder", "encode", [text, ..]) => Value::string(url_encode(&text.to_string())),
            ("URLDecoder", "decode", [text, ..]) => Value::string(url_decode(&text.to_string())),
            ("Base64", "getEncoder", []) => text_object("java.util.Base64$Encoder", ""),
            ("Base64", "getDecoder", []) => text_object("java.util.Base64$Decoder", ""),
            ("Math", "random", []) => {
                let bits = uuid::Uuid::new_v4().as_u64_pair().0 >> 11;
                Value::Number(Number::Double(bits as f64 / (1u64 << 53) as f64))
            }
            ("Paths", "get", [first, rest @ ..]) => {
                let mut text = first.to_string();
                for part in rest {
                    text = format!("{}/{part}", text.trim_end_matches('/'));
                }
                Value::Path(std::rc::Rc::new(crate::value::PathValue::parse(
                    &text,
                    &self.host.launch_dir(),
                )))
            }
            ("Files", "exists" | "isDirectory" | "isRegularFile", [Value::Path(path), ..]) => {
                let metadata = self
                    .host
                    .metadata(path)
                    .map_err(|error| error.or_at(span))?;
                Value::Boolean(match name {
                    "exists" => metadata.is_some(),
                    "isDirectory" => metadata.is_some_and(|file| file.directory),
                    _ => metadata.is_some_and(|file| !file.directory),
                })
            }
            ("ZoneId", "systemDefault", []) => text_object("java.time.ZoneId", "system"),
            ("MessageDigest", "getInstance", [algorithm]) => {
                super::bytes::message_digest(&algorithm.to_string()).at(span)?
            }
            ("Arrays", "asList", [Value::List(list)]) => Value::list(list.borrow().items.clone()),
            ("Arrays", "asList", items) => Value::list(items.to_vec()),
            ("Files", "size", [Value::Path(path)]) => match self.host.metadata(path) {
                Ok(Some(metadata)) => Value::Number(Number::Long(metadata.size as i64)),
                Ok(None) => {
                    let exception =
                        crate::exception::Exception::new("NoSuchFileException", path.text());
                    return Err(super::super::thrown(exception, span));
                }
                Err(error) => return Err(error.or_at(span)),
            },
            ("Files", "newInputStream", [Value::Path(path)]) => {
                let bytes = self.host.read(path).map_err(|error| error.or_at(span))?;
                super::stream(bytes)
            }
            ("FilesEx", "copyTo", [source @ Value::Path(_), target]) => {
                let copied = self.call_method(source, "copyTo", vec![target.clone()], span)?;
                return Ok(Some(copied));
            }
            // The functions of every script are static methods of `nextflow.Nextflow`, which
            // Groovy classes call by that name.
            ("Nextflow", "file" | "files" | "error" | "tuple" | "groupKey", _) => {
                return self.call_builtin(name, arguments.to_vec(), span).map(Some);
            }
            ("SplitterEx", "splitCsv", [source, options @ ..]) => {
                return self
                    .call_method(source, "splitCsv", options.to_vec(), span)
                    .map(Some);
            }
            ("SchemaLoader", "load", [json]) => super::org_json::load_schema(self, json, span)?,
            // Java's parsers of numbers, like `Integer.parseInt`, as Groovy's `toInteger()`.
            (
                "Integer" | "Long" | "Double" | "Float",
                "parseInt" | "parseLong" | "parseDouble" | "parseFloat" | "valueOf",
                [Value::String(text) | Value::GString(text)],
            ) => {
                let method = format!("to{class}");
                super::string::parse_number(&method, text).at(span)?
            }
            ("JsonOutput", "toJson", [value]) => {
                Value::string(self.json_text(value, &JsonOptions::default(), span)?)
            }
            ("JsonOutput", "prettyPrint", [text]) => {
                Value::string(json::pretty_print(&text.to_string()).at(span)?)
            }
            // `System.exit(code)` ends the run with the code, as the legacy `exit code` does.
            ("System", "exit", [code @ Value::Number(_)]) => {
                return self
                    .call_builtin("exit", vec![code.clone()], span)
                    .map(Some);
            }
            ("Runtime", "getRuntime", []) => Value::Namespace("Runtime.runtime".into()),
            ("String", "valueOf", [value]) => Value::string(value.to_string()),
            ("String", "join", [separator, items @ ..]) if !items.is_empty() => {
                let items = match items {
                    [single @ (Value::List(_) | Value::Set(_))] => super::elements(single),
                    several => several.to_vec(),
                };
                let texts: Vec<String> = items.iter().map(Value::to_string).collect();
                Value::string(texts.join(&separator.to_string()))
            }
            // Java's `Boolean.parseBoolean`: only `true` in any case is true.
            ("Boolean", "parseBoolean" | "valueOf", [Value::Boolean(value)]) => {
                Value::Boolean(*value)
            }
            ("Boolean", "parseBoolean" | "valueOf", [text]) => {
                Value::Boolean(text.to_string().eq_ignore_ascii_case("true"))
            }
            ("Integer" | "Long", "toString", [Value::Number(number)]) => {
                Value::string(number.to_i64().to_string())
            }
            ("Integer" | "Long", "toString", [Value::Number(number), Value::Number(radix)]) => {
                Value::string(radix_text(number.to_i64(), radix.to_i64() as u32))
            }
            // The digits of the unsigned 32 or 64 bits, as Java gives them for negatives.
            (
                "Integer" | "Long",
                "toHexString" | "toBinaryString" | "toOctalString",
                [Value::Number(number)],
            ) => {
                let bits = match class {
                    "Integer" => u64::from(number.to_i64() as i32 as u32),
                    _ => number.to_i64() as u64,
                };
                Value::string(match name {
                    "toHexString" => format!("{bits:x}"),
                    "toBinaryString" => format!("{bits:b}"),
                    _ => format!("{bits:o}"),
                })
            }
            // Nextflow's `Duration.of` and `MemoryUnit.of`: text, or milliseconds and bytes.
            ("Duration", "of", [Value::Number(millis)]) => Value::Duration(millis.to_i64()),
            ("Duration", "of", [text]) => {
                Value::Duration(crate::units::parse_duration(&text.to_string()).at(span)?)
            }
            ("MemoryUnit", "of", [Value::Number(bytes)]) => Value::MemoryUnit(bytes.to_i64()),
            ("MemoryUnit", "of", [text]) => {
                Value::MemoryUnit(crate::units::parse_memory(&text.to_string()).at(span)?)
            }
            ("System", "getenv", [variable]) => self
                .host
                .environment(&variable.to_string())
                .map_or(Value::Null, Value::string),
            ("System", "getProperty", [property]) => property_value(&property.to_string()),
            ("System", "currentTimeMillis", []) => Value::Number(Number::Long(crate::date::now())),
            ("Math", _, [Value::Number(a), Value::Number(b)]) => match math2(name, a, b) {
                Some(value) => value,
                None => return Ok(None),
            },
            ("Math", _, [Value::Number(a)]) => match math1(name, a) {
                Some(value) => value,
                None => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }
}

impl Interpreter<'_> {
    /// `Class.name`: a constant like `Integer.MAX_VALUE`, or a property of the class itself.
    pub(in crate::interpreter) fn static_property(
        &mut self,
        class: &str,
        name: &str,
    ) -> Option<Value> {
        let number = |number| Some(Value::Number(number));
        match (class, name) {
            ("Integer", "MAX_VALUE") => number(Number::Integer(i32::MAX)),
            ("DateTimeFormatter", "ISO_LOCAL_DATE") => Some(text_object(
                "java.time.format.DateTimeFormatter",
                "yyyy-MM-dd",
            )),
            ("DateTimeFormatter", "ISO_LOCAL_DATE_TIME") => Some(text_object(
                "java.time.format.DateTimeFormatter",
                "yyyy-MM-dd'T'HH:mm:ss",
            )),
            // A `RoundingMode` is its name, which `setScale` reads.
            (
                "RoundingMode",
                "UP" | "DOWN" | "CEILING" | "FLOOR" | "HALF_UP" | "HALF_DOWN" | "HALF_EVEN"
                | "UNNECESSARY",
            ) => Some(Value::string(name)),
            ("DayOfWeek", day) => {
                let day = crate::value::DAYS.iter().position(|name| *name == day)?;
                Some(super::time::time_value(Time::Day(day as u8 + 1)))
            }
            ("Integer", "MIN_VALUE") => number(Number::Integer(i32::MIN)),
            ("Double", "POSITIVE_INFINITY") => number(Number::Double(f64::INFINITY)),
            ("Double", "NEGATIVE_INFINITY") => number(Number::Double(f64::NEG_INFINITY)),
            ("Double", "NaN") => number(Number::Double(f64::NAN)),
            ("Double", "MAX_VALUE") => number(Number::Double(f64::MAX)),
            ("Long", "MAX_VALUE") => number(Number::Long(i64::MAX)),
            ("Long", "MIN_VALUE") => number(Number::Long(i64::MIN)),
            // The separators of paths on Linux, where tasks run.
            ("File", "separator") => Some(Value::string("/")),
            ("File", "pathSeparator") => Some(Value::string(":")),
            // The kinds of files that `eachFile` and `eachFileRecurse` visit.
            ("FileType", "FILES" | "DIRECTORIES" | "ANY") => Some(Value::string(name)),
            ("Locale", "ROOT" | "US" | "UK" | "ENGLISH") => {
                let tag = match name {
                    "ROOT" => "",
                    "US" => "en_US",
                    "UK" => "en_GB",
                    _ => "en",
                };
                Some(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
                    crate::value::Object {
                        class: "java.util.Locale",
                        state: State::Locale(tag.into()),
                    },
                ))))
            }
            // `System.out` and `System.err` print through the host.
            ("System", "out" | "err") => Some(Value::Namespace(format!("System.{name}").into())),
            ("Runtime", "runtime") => Some(Value::Namespace("Runtime.runtime".into())),
            // The metaclass of a class, whose new methods here reach no values, as Groovy's
            // metaclass of an interface like `Map` does not reach its objects.
            (_, "metaClass") => Some(Value::Namespace(format!("metaClass:{class}").into())),
            (_, "name") => crate::value::qualified_class_name(class).map(Value::string),
            (_, "simpleName") => {
                crate::value::qualified_class_name(class).map(|_| Value::string(class))
            }
            _ => None,
        }
    }
}

/// `System.getProperty` for the properties that pipelines read.
fn property_value(name: &str) -> Value {
    let variable = |name: &str| std::env::var(name).ok();
    let value = match name {
        "user.name" => variable("USER"),
        "user.home" => variable("HOME"),
        "user.dir" => std::env::current_dir()
            .ok()
            .map(|dir| dir.to_string_lossy().into_owned()),
        "java.io.tmpdir" => Some("/tmp".into()),
        "line.separator" => Some("\n".into()),
        "file.separator" => Some("/".into()),
        _ => None,
    };
    value.map_or(Value::Null, Value::string)
}

/// Java's overloads by argument type: `int` if both are, else `long`, else `double`.
fn math2(name: &str, a: &Number, b: &Number) -> Option<Value> {
    // Groovy finds `Math.max(1, 2.5)` ambiguous, so big numbers are left out.
    let big = |n: &Number| matches!(n, Number::BigInteger(_) | Number::BigDecimal(_));
    if big(a) || big(b) {
        return None;
    }
    let integral = |n: &Number| matches!(n, Number::Integer(_) | Number::Long(_));
    let number = match name {
        "max" | "min" => {
            let pick_first = if name == "max" {
                a.to_f64() >= b.to_f64()
            } else {
                a.to_f64() <= b.to_f64()
            };
            match (a, b) {
                (Number::Integer(_), Number::Integer(_)) => {
                    let value = if pick_first { a } else { b };
                    Number::Integer(value.to_i32())
                }
                _ if integral(a) && integral(b) => {
                    Number::Long(if pick_first { a.to_i64() } else { b.to_i64() })
                }
                _ => Number::Double(if name == "max" {
                    a.to_f64().max(b.to_f64())
                } else {
                    a.to_f64().min(b.to_f64())
                }),
            }
        }
        "pow" => Number::Double(a.to_f64().powf(b.to_f64())),
        _ => return None,
    };
    Some(Value::Number(number))
}

fn math1(name: &str, a: &Number) -> Option<Value> {
    let value = a.to_f64();
    let number = match name {
        "ceil" => Number::Double(value.ceil()),
        "floor" => Number::Double(value.floor()),
        "sqrt" => Number::Double(value.sqrt()),
        "log" => Number::Double(value.ln()),
        "log10" => Number::Double(value.log10()),
        "exp" => Number::Double(value.exp()),
        "round" => match a {
            Number::Float(value) => Number::Integer(java_round(f64::from(*value)) as i32),
            _ => Number::Long(java_round(value)),
        },
        "abs" => match a {
            Number::Integer(value) => Number::Integer(value.wrapping_abs()),
            Number::Long(value) => Number::Long(value.wrapping_abs()),
            _ => Number::Double(value.abs()),
        },
        _ => return None,
    };
    Some(Value::Number(number))
}

/// An object of a Java class that is known by its text, like the pattern of a formatter.
pub(in crate::interpreter) fn text_object(class: &'static str, text: &str) -> Value {
    Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
        crate::value::Object {
            class,
            state: State::Text(text.into()),
        },
    )))
}

fn uuid_object(value: uuid::Uuid) -> Value {
    Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
        crate::value::Object {
            class: "java.util.UUID",
            state: State::Uuid(value),
        },
    )))
}

fn parse_uuid(text: &str) -> Result<uuid::Uuid, crate::exception::Exception> {
    let parts: Vec<_> = text.split('-').collect();
    if text.len() > 36 || parts.len() != 5 {
        return Err(crate::exception::Exception::new(
            "IllegalArgumentException",
            format!("Invalid UUID string: {text}"),
        ));
    }
    let parts: Vec<_> = parts
        .iter()
        .map(|part| {
            i64::from_str_radix(part, 16)
                .map(|value| value as u64)
                .map_err(|_| {
                    crate::exception::Exception::new(
                        "NumberFormatException",
                        format!("Invalid UUID component: {part}"),
                    )
                })
        })
        .collect::<Result<_, _>>()?;
    Ok(uuid::Uuid::from_u64_pair(
        ((parts[0] & 0xffffffff) << 32) | ((parts[1] & 0xffff) << 16) | (parts[2] & 0xffff),
        ((parts[3] & 0xffff) << 48) | (parts[4] & 0xffffffffffff),
    ))
}

/// Java's `Long.toString(value, radix)`: lowercase digits with a minus sign.
fn radix_text(value: i64, radix: u32) -> String {
    let radix = if (2..=36).contains(&radix) { radix } else { 10 };
    let mut magnitude = value.unsigned_abs();
    let mut digits = Vec::new();
    loop {
        let digit = (magnitude % u64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).unwrap_or('0'));
        magnitude /= u64::from(radix);
        if magnitude == 0 {
            break;
        }
    }
    if value < 0 {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// Java's `URLEncoder.encode` for UTF-8: letters, digits and `.-*_` stay, a space becomes `+`
/// and every other byte `%XX`.
fn url_encode(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// Java's `URLDecoder.decode` for UTF-8: `+` is a space and `%XX` a byte.
fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = match bytes[index] {
            b'+' => b' ',
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        index += 2;
                        byte
                    }
                    Err(_) => b'%',
                }
            }
            other => other,
        };
        decoded.push(byte);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
