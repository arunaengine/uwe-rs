//! Objects of Java classes that iunctus stands in for, like `groovy.json.JsonSlurper`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use super::Closure;

/// An object of a stood-in class.
#[derive(Debug)]
pub struct Object {
    /// The fully qualified class name.
    pub class: &'static str,
    pub state: State,
}

#[derive(Debug)]
pub enum State {
    /// `groovy.json.JsonGenerator.Options`, a builder, or the `JsonGenerator` it builds.
    Json(JsonOptions),
    /// `groovy.json.JsonSlurper`
    JsonSlurper,
    /// `nextflow.script.WorkflowMetadata`, the `workflow` of a run.
    Workflow(Workflow),
    /// `groovy.text.SimpleTemplateEngine`
    TemplateEngine,
    /// `groovy.util.ConfigSlurper`
    ConfigSlurper,
    /// `org.yaml.snakeyaml.Yaml`
    Yaml,
    /// `java.net.URI`, by its text.
    Uri(Rc<str>),
    /// `java.util.UUID`, with immutable identifier bits.
    Uuid(uuid::Uuid),
    /// A value of `java.time`: a date, a date and time, an instant or a duration.
    Time(Time),
    /// An input stream or reader over bytes that were read at once, as `newInputStream()`,
    /// `GZIPInputStream` and readers give it, with the position of the next byte.
    Stream(Rc<[u8]>, usize),
    /// `nextflow.extension.GroupKey` from `groupKey(key, size)`: the key, which it stands for,
    /// with the size of its group for `groupTuple`.
    GroupKey(super::Value, i32),
    /// A template of the engine, with its text.
    Template(Rc<str>),
    /// `java.util.Locale`, by its tag like `en_US`.
    Locale(Rc<str>),
    /// `byte[]`, which like every Java array is only equal to itself.
    Bytes(Rc<[u8]>),
    /// The writer of `path.withWriter { w -> ... }`, with the text written so far.
    Writer(String),
    /// `java.lang.StringBuilder` or `StringBuffer`, with its text.
    Builder(String),
    /// A `java.util.Iterator` over a copy of the items, with the position of the next one.
    Iterator(Vec<super::Value>, usize),
    /// `java.util.Random` with the state of its generator.
    Random(i64),
    /// `java.util.Optional`, empty when it holds null.
    Optional(super::Value),
    /// `java.security.MessageDigest`, by its algorithm.
    Digest(&'static str),
    /// An object of a plugin class that is known by its text, like fgbio's `ReadStructure`.
    Text(Rc<str>),
    /// A Groovy class of the library of the pipeline, as its name gives it in code.
    Class(Rc<crate::lowered::LibraryClass>),
    /// A constant of an enum of the library; each constant is one object.
    EnumConstant {
        class: Rc<crate::lowered::LibraryClass>,
        name: Rc<str>,
        ordinal: usize,
    },
    /// `org.json.JSONObject`: a map, whose objects and arrays are `JSONObject` and
    /// `JSONArray` too.
    JsonObject(super::Value),
    /// `org.json.JSONArray`: a list.
    JsonArray(super::Value),
    /// `org.everit.json.schema.Schema`, by the JSON text of the schema.
    JsonSchema(Rc<str>),
    /// An object of a class of the library, with its fields.
    Instance {
        class: Rc<crate::lowered::LibraryClass>,
        fields: indexmap::IndexMap<Rc<str>, super::Value>,
    },
}

/// The values of `java.time` that scripts use for timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Time {
    /// `LocalDate`
    Date(jiff::civil::Date),
    /// `LocalDateTime`
    DateTime(jiff::civil::DateTime),
    /// `Instant`
    Instant(jiff::Timestamp),
    /// `java.time.Duration`
    Duration(jiff::SignedDuration),
}

impl Time {
    /// The text of a `java.time` value, as its `toString()` gives it: ISO dates and times, with
    /// seconds and fractions only when they are not zero, and durations like `PT1H2M5S`.
    pub fn text(self) -> String {
        let fraction = |nanos: i32| match nanos {
            0 => String::new(),
            nanos if nanos % 1_000_000 == 0 => format!(".{:03}", nanos / 1_000_000),
            nanos if nanos % 1_000 == 0 => format!(".{:06}", nanos / 1_000),
            nanos => format!(".{nanos:09}"),
        };
        match self {
            Time::Date(date) => date.strftime("%Y-%m-%d").to_string(),
            Time::DateTime(time) => {
                let base = time.strftime("%Y-%m-%dT%H:%M").to_string();
                match (time.second(), time.subsec_nanosecond()) {
                    (0, 0) => base,
                    (second, nanos) => format!("{base}:{second:02}{}", fraction(nanos)),
                }
            }
            Time::Instant(instant) => {
                let utc = instant.to_zoned(jiff::tz::TimeZone::UTC);
                let base = utc.strftime("%Y-%m-%dT%H:%M:%S").to_string();
                format!("{base}{}Z", fraction(instant.subsec_nanosecond()))
            }
            Time::Duration(duration) => {
                let seconds = duration.as_secs();
                if seconds == 0 && duration.subsec_nanos() == 0 {
                    return "PT0S".into();
                }
                let (hours, minutes, rest) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
                let mut text = String::from("PT");
                if hours != 0 {
                    text.push_str(&format!("{hours}H"));
                }
                if minutes != 0 {
                    text.push_str(&format!("{minutes}M"));
                }
                if rest != 0 || duration.subsec_nanos() != 0 {
                    text.push_str(&format!("{rest}{}S", fraction(duration.subsec_nanos())));
                }
                text
            }
        }
    }
}

/// The `workflow` of a run: its properties, and the handlers that run when it ends.
#[derive(Debug)]
pub struct Workflow {
    /// A map of the properties, like `runName` and `manifest`.
    pub properties: super::Value,
    /// The closures of `workflow.onComplete` and `workflow.onError`, in order.
    pub on_complete: Vec<Rc<Closure>>,
    pub on_error: Vec<Rc<Closure>>,
}

/// How a JSON generator writes values.
#[derive(Clone, Debug, Default)]
pub struct JsonOptions {
    pub exclude_nulls: bool,
    /// Closures that turn an instance of a class, by simple name, into another value.
    pub converters: Vec<(Rc<str>, Rc<Closure>)>,
}
