//! Bounded Boa expression transport and the container worker entrypoint.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use executor::CancellationToken;
use executor::process::{ContainerProcess, ContainerSpec, ProcessLimits};
use serde::Deserialize;

use crate::javascript::thread::HelperProcess;
use crate::value::Value;

pub(crate) const FRAME_LIMIT: usize = 16 << 20;
pub(crate) const NESTING_LIMIT: usize = 64;
const TIME_LIMIT: Duration = Duration::from_secs(20);
const BOUNDARY: &str = "CWL JavaScript execution failure: ";

struct Helper {
    process: HelperProcess,
    cancel: CancellationToken,
}

/// The image and engine of the isolated JavaScript helper, and the run's cancellation.
#[derive(Clone)]
pub struct JavaScriptSettings {
    pub engine: String,
    pub image: String,
    pub cancel: CancellationToken,
    helper: Arc<Mutex<Option<Helper>>>,
    /// Whether the helper runs the image of the running binary, made when it is missing,
    /// instead of `image`.
    own: bool,
}

impl Default for JavaScriptSettings {
    fn default() -> Self {
        Self {
            engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_default(),
            image: std::env::var("IUNCTUS_CWL_JS_IMAGE")
                .unwrap_or_else(|_| "iunctus-cwl-js:0.1.0".into()),
            cancel: CancellationToken::new(),
            helper: Arc::default(),
            own: false,
        }
    }
}

impl fmt::Debug for JavaScriptSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JavaScriptSettings")
            .field("engine", &self.engine)
            .field("image", &self.image)
            .field("own", &self.own)
            .finish()
    }
}

impl PartialEq for JavaScriptSettings {
    fn eq(&self, other: &Self) -> bool {
        self.engine == other.engine && self.image == other.image && self.own == other.own
    }
}

#[derive(Debug)]
pub(crate) enum Failure {
    Language(String),
    Boundary(String),
}

impl Failure {
    pub(crate) fn text(self) -> String {
        match self {
            Self::Language(text) => text,
            Self::Boundary(text) => format!("{BOUNDARY}{text}"),
        }
    }
}

pub(crate) fn boundary(error: &str) -> bool {
    error.contains(BOUNDARY)
}

struct Watchdog(mpsc::Sender<()>, Option<thread::JoinHandle<()>>);

impl Watchdog {
    fn start(limit: Duration, expired: impl FnOnce() + Send + 'static) -> Self {
        let (stop, wait) = mpsc::channel();
        let task = thread::spawn(move || {
            if matches!(
                wait.recv_timeout(limit),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                expired();
            }
        });
        Self(stop, Some(task))
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
        if let Some(task) = self.1.take() {
            let _ = task.join();
        }
    }
}

struct Capped {
    bytes: Vec<u8>,
    cancel: CancellationToken,
    deadline: Instant,
}

impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("evaluation cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::other("evaluation deadline exceeded"));
        }
        if bytes.len() > FRAME_LIMIT.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("expression frame exceeded 16 MiB"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Quoted<'a>(&'a mut Capped);

impl Write for Quoted<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for &byte in bytes {
            match byte {
                b'"' | b'\\' => self.0.write_all(&[b'\\', byte])?,
                0..=31 => write!(self.0, "\\u{byte:04x}")?,
                _ => self.0.write_all(&[byte])?,
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_string(writer: &mut impl Write, text: &str) -> io::Result<()> {
    writer.write_all(b"\"")?;
    for character in text.chars() {
        match character {
            '"' => writer.write_all(b"\\\"")?,
            '\\' => writer.write_all(b"\\\\")?,
            '\n' => writer.write_all(b"\\n")?,
            '\r' => writer.write_all(b"\\r")?,
            '\t' => writer.write_all(b"\\t")?,
            '\u{8}' => writer.write_all(b"\\b")?,
            '\u{c}' => writer.write_all(b"\\f")?,
            '\u{2028}' | '\u{2029}' => write!(writer, "\\u{:04x}", character as u32)?,
            c if (c as u32) < 0x20 => write!(writer, "\\u{:04x}", c as u32)?,
            c => writer.write_all(c.encode_utf8(&mut [0; 4]).as_bytes())?,
        }
    }
    writer.write_all(b"\"")
}

fn write_value(writer: &mut impl Write, value: &Value, depth: usize) -> io::Result<()> {
    if depth > NESTING_LIMIT {
        return Err(io::Error::other("expression value nesting exceeds 64"));
    }
    match value {
        Value::Null => writer.write_all(b"null"),
        Value::Bool(value) => writer.write_all(if *value { b"true" } else { b"false" }),
        Value::Integer(value) => write!(writer, "{value}"),
        Value::BigInteger(value) => writer.write_all(value.as_bytes()),
        Value::Float(value) => {
            let text = if value.is_nan() {
                "NaN".into()
            } else if *value == f64::INFINITY {
                "Infinity".into()
            } else if *value == f64::NEG_INFINITY {
                "-Infinity".into()
            } else {
                crate::value::javascript_float(*value)
            };
            writer.write_all(text.as_bytes())
        }
        Value::String(value) => json_string(writer, value),
        Value::List(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    writer.write_all(b",")?;
                }
                write_value(writer, value, depth + 1)?;
            }
            writer.write_all(b"]")
        }
        Value::Map(values) => {
            writer.write_all(b"{")?;
            for (index, (name, value)) in values.iter().enumerate() {
                if index != 0 {
                    writer.write_all(b",")?;
                }
                json_string(writer, name)?;
                writer.write_all(b":")?;
                write_value(writer, value, depth + 1)?;
            }
            writer.write_all(b"}")
        }
    }
}

pub(crate) fn check_frame(text: &str, depth_limit: usize) -> Result<(), String> {
    if text.len() > FRAME_LIMIT {
        return Err("expression frame exceeded 16 MiB".into());
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > depth_limit {
                        return Err("expression result nesting exceeds 64".into());
                    }
                }
                b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

impl JavaScriptSettings {
    pub(crate) fn bind_cancel(&mut self, cancel: CancellationToken) {
        self.cancel = cancel;
        self.helper = Arc::default();
    }

    pub fn new(engine: String, cancel: CancellationToken) -> Self {
        Self {
            engine,
            cancel,
            ..Self::default()
        }
    }

    /// Settings whose helper runs the running iunctus binary, in an image that is made from
    /// it when the engine lacks it; `IUNCTUS_CWL_JS_IMAGE` names another image instead.
    pub fn from_binary(engine: String, cancel: CancellationToken) -> Self {
        let own = std::env::var_os("IUNCTUS_CWL_JS_IMAGE").is_none();
        Self {
            own,
            ..Self::new(engine, cancel)
        }
    }

    pub(crate) fn evaluate(
        &self,
        library: &[String],
        variables: &[(&str, &Value)],
        body: &str,
    ) -> Result<Value, Failure> {
        // The first build of the image may take longer than an evaluation may.
        let image = match self.own {
            true => crate::javascript::image::own_image(&self.engine, &self.cancel)
                .map_err(Failure::Boundary)?,
            false => self.image.clone(),
        };
        let deadline = Instant::now() + TIME_LIMIT;
        crate::javascript::thread::run(|| {
            self.evaluate_blocking(&image, library, variables, body, deadline)
        })?
    }

    fn evaluate_blocking(
        &self,
        image: &str,
        library: &[String],
        variables: &[(&str, &Value)],
        body: &str,
        deadline: Instant,
    ) -> Result<Value, Failure> {
        let mut request = Capped {
            bytes: Vec::new(),
            cancel: self.cancel.clone(),
            deadline,
        };
        let mut encode = || -> io::Result<()> {
            request.write_all(b"{\"library\":")?;
            serde_json::to_writer(&mut request, library).map_err(io::Error::other)?;
            request.write_all(b",\"variables\":[")?;
            for (index, (name, value)) in variables.iter().enumerate() {
                if index != 0 {
                    request.write_all(b",")?;
                }
                request.write_all(b"[")?;
                json_string(&mut request, name)?;
                request.write_all(b",\"")?;
                write_value(&mut Quoted(&mut request), value, 0)?;
                request.write_all(b"\"]")?;
            }
            request.write_all(b"],\"body\":")?;
            json_string(&mut request, body)?;
            request.write_all(b"}")
        };
        encode().map_err(|error| Failure::Boundary(error.to_string()))?;
        let request = String::from_utf8(request.bytes)
            .map_err(|error| Failure::Boundary(error.to_string()))?;
        let mut helper = loop {
            if self.cancel.is_cancelled() {
                return Err(Failure::Boundary("evaluation cancelled".into()));
            }
            if Instant::now() >= deadline {
                return Err(Failure::Boundary("evaluation deadline exceeded".into()));
            }
            match self.helper.try_lock() {
                Ok(helper) => break helper,
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
                Err(error) => return Err(Failure::Boundary(error.to_string())),
            }
        };
        let cancel = helper
            .as_ref()
            .map(|helper| helper.cancel.clone())
            .unwrap_or_else(|| self.cancel.child_token());
        let timed = cancel.clone();
        let _watchdog = Watchdog::start(
            deadline.saturating_duration_since(Instant::now()),
            move || timed.cancel(),
        );
        if helper.is_none() {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let spec = ContainerSpec {
                engine: self.engine.clone(),
                image: image.to_owned(),
                name: format!(
                    "iunctus-cwl-js-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ),
                entrypoint: Some("/usr/local/bin/iunctus".into()),
                command: vec!["__cwl-js-worker".into()],
                environment: vec![("IUNCTUS_CWL_WORKER".into(), "1".into())],
                memory: Some(256 << 20),
                ..ContainerSpec::default()
            };
            let process = ContainerProcess::start_bounded(
                &spec,
                ProcessLimits {
                    max_frame_bytes: FRAME_LIMIT,
                    reply_capacity: 1,
                },
                &cancel,
            )
            .map_err(|error| Failure::Boundary(error.to_string()))?;
            *helper = Some(Helper {
                process: HelperProcess::new(process),
                cancel,
            });
        }
        let reply = helper
            .as_mut()
            .expect("helper was started")
            .process
            .exchange_with_timeout(&request, deadline.saturating_duration_since(Instant::now()));
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                helper.take();
                return Err(Failure::Boundary(error.to_string()));
            }
        };
        if self.cancel.is_cancelled() || Instant::now() >= deadline {
            helper.take();
            return Err(Failure::Boundary(
                "evaluation cancelled or deadline exceeded".into(),
            ));
        }
        check_frame(&reply, NESTING_LIMIT + 1).map_err(Failure::Boundary)?;
        let mut json = serde_json::Deserializer::from_str(&reply);
        serde::de::IgnoredAny::deserialize(&mut json)
            .and_then(|_| json.end())
            .map_err(|error| Failure::Boundary(error.to_string()))?;
        let Value::Map(mut reply) = Value::parse(&reply).map_err(Failure::Boundary)? else {
            return Err(Failure::Boundary("invalid worker reply".into()));
        };
        if self.cancel.is_cancelled() || Instant::now() >= deadline {
            helper.take();
            return Err(Failure::Boundary(
                "evaluation cancelled or deadline exceeded".into(),
            ));
        }
        if reply.len() != 1 {
            return Err(Failure::Boundary("invalid worker reply".into()));
        }
        if let Some(value) = reply.shift_remove("value") {
            return Ok(value);
        }
        if let Some(Value::String(error)) = reply.shift_remove("error") {
            return Err(Failure::Language(error));
        }
        if let Some(Value::String(error)) = reply.shift_remove("boundary") {
            return Err(Failure::Boundary(error));
        }
        Err(Failure::Boundary("invalid worker reply".into()))
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;

/// Runs Boa inside the bounded helper container. Called by the hidden binary dispatch.
pub fn javascript_worker_main() -> Result<(), String> {
    crate::javascript::budget::prove_worker_budget()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut first = [0];
        match input.read_exact(&mut first) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
        let _watchdog = Watchdog::start(TIME_LIMIT, || std::process::exit(124));
        let mut frame = vec![first[0]];
        Read::by_ref(&mut input)
            .take(FRAME_LIMIT as u64)
            .read_until(b'\n', &mut frame)
            .map_err(|error| error.to_string())?;
        if frame.last() != Some(&b'\n') || frame.len() > FRAME_LIMIT + 1 {
            return Err("worker request exceeded frame limit or ended incompletely".into());
        }
        frame.pop();
        let text = std::str::from_utf8(&frame).map_err(|error| error.to_string())?;
        check_frame(text, NESTING_LIMIT)?;
        let request: serde_json::Value =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        let library = request["library"]
            .as_array()
            .ok_or("invalid worker library")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("invalid library entry")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let variables = request["variables"]
            .as_array()
            .ok_or("invalid worker variables")?
            .iter()
            .map(|value| {
                Ok((
                    value[0].as_str().ok_or("invalid variable name")?,
                    value[1].as_str().ok_or("invalid variable value")?,
                ))
            })
            .collect::<Result<Vec<_>, &str>>()?;
        let body = request["body"].as_str().ok_or("invalid worker body")?;
        let result = crate::javascript::evaluate_literals(&library, &variables, body);
        let mut reply = Capped {
            bytes: Vec::new(),
            cancel: CancellationToken::new(),
            deadline: Instant::now() + TIME_LIMIT,
        };
        match result {
            Ok(value) => {
                reply
                    .write_all(b"{\"value\":")
                    .map_err(|error| error.to_string())?;
                write_value(&mut reply, &value, 0).map_err(|error| error.to_string())?;
            }
            Err(error) => {
                let (key, error) = match error {
                    Failure::Language(error) => (b"{\"error\":".as_slice(), error),
                    Failure::Boundary(error) => (b"{\"boundary\":".as_slice(), error),
                };
                reply.write_all(key).map_err(|error| error.to_string())?;
                json_string(&mut reply, &error).map_err(|error| error.to_string())?;
            }
        }
        reply.write_all(b"}").map_err(|error| error.to_string())?;
        let text = std::str::from_utf8(&reply.bytes).map_err(|error| error.to_string())?;
        check_frame(text, NESTING_LIMIT + 1)?;
        output
            .write_all(&reply.bytes)
            .and_then(|()| output.write_all(b"\n"))
            .and_then(|()| output.flush())
            .map_err(|error| error.to_string())?;
    }
}
