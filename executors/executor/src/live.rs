//! Sends the output and resource use of an attempt while it runs, without waiting for listeners.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustix::fs::{FileType, Mode, OFlags};

/// The largest chunk of output that one event carries, in bytes.
pub const CHUNK: usize = 4096;

/// How often a backend looks for new output and resource use of a running attempt.
pub const PERIOD: Duration = Duration::from_secs(1);

/// The most bytes of one stream that one look reads, so that a task that writes fast cannot
/// fill the memory of this process; the rest comes with the next look.
const READ_LIMIT: u64 = 1 << 20;

/// A stream of the output of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// What a running attempt tells while it runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Live {
    /// New output of the task: whole lines, at most [`CHUNK`] bytes.
    Log { stream: LogStream, chunk: String },
    /// What the container of the task uses now: its CPU share, where 100 is one whole CPU,
    /// and its memory in bytes.
    Usage {
        cpu_percent: Option<f64>,
        memory_bytes: Option<u64>,
    },
}

/// Where a backend sends what one attempt tells while it runs. It must return at once.
#[derive(Clone)]
pub struct LiveSink(Arc<dyn Fn(Live) + Send + Sync>);

impl LiveSink {
    pub fn new(sink: impl Fn(Live) + Send + Sync + 'static) -> LiveSink {
        LiveSink(Arc::new(sink))
    }

    pub fn send(&self, live: Live) {
        (self.0)(live);
    }

    /// Sends text of a stream in chunks of whole lines.
    pub fn log(&self, stream: LogStream, text: &str) {
        for chunk in chunks(text) {
            self.send(Live::Log {
                stream,
                chunk: chunk.to_owned(),
            });
        }
    }
}

impl std::fmt::Debug for LiveSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LiveSink")
    }
}

/// Two sinks are equal when they share their function.
impl PartialEq for LiveSink {
    fn eq(&self, other: &LiveSink) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LiveSink {}

/// The function that gets what the attempts of a run tell.
type Feed = Arc<dyn Fn(u64, u32, Live) + Send + Sync>;

/// Where all attempts of a run send what they tell, with the number of the task and attempt.
#[derive(Clone)]
pub struct LiveFeed {
    feed: Feed,
    /// Values that the output of a task shows as `[secret]`, the longest first.
    secrets: Arc<[String]>,
}

impl LiveFeed {
    /// A feed that calls `feed` itself, which must return at once.
    pub fn new(feed: impl Fn(u64, u32, Live) + Send + Sync + 'static) -> LiveFeed {
        LiveFeed {
            feed: Arc::new(feed),
            secrets: Arc::from([]),
        }
    }

    /// This feed, with each of `secrets` in the output of a task as `[secret]` before it
    /// leaves the sink of the attempt, also a value that the end of a chunk splits.
    pub fn hiding(mut self, secrets: impl IntoIterator<Item = String>) -> LiveFeed {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        secrets.dedup();
        self.secrets = secrets.into();
        self
    }

    /// A feed that calls `feed` on a thread of its own, through a queue of `capacity` events.
    /// An event that finds the queue full is dropped, so a slow listener never holds a task.
    pub fn bounded(
        capacity: usize,
        feed: impl Fn(u64, u32, Live) + Send + 'static,
    ) -> std::io::Result<LiveFeed> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<(u64, u32, Live)>(capacity);
        std::thread::Builder::new()
            .name("uwe-live".into())
            .spawn(move || {
                // The thread ends when the last sink of the feed is gone.
                for (task, attempt, live) in receiver {
                    feed(task, attempt, live);
                }
            })?;
        Ok(LiveFeed::new(move |task, attempt, live| {
            let _ = sender.try_send((task, attempt, live));
        }))
    }

    /// The sink of one attempt of a task. With secrets, it holds back the end of a stream
    /// that may start one, until more output or the end of the attempt drops the sink.
    pub fn sink(&self, task: u64, attempt: u32) -> LiveSink {
        let feed = self.feed.clone();
        if self.secrets.is_empty() {
            return LiveSink::new(move |live| feed(task, attempt, live));
        }
        let hiding = Hiding {
            feed,
            task,
            attempt,
            secrets: self.secrets.clone(),
            held: Mutex::default(),
        };
        LiveSink::new(move |live| hiding.send(live))
    }
}

/// The secrets of the output of one attempt: what each stream held back, as it may start one.
struct Hiding {
    feed: Feed,
    task: u64,
    attempt: u32,
    secrets: Arc<[String]>,
    held: Mutex<[String; 2]>,
}

impl Hiding {
    fn send(&self, live: Live) {
        let Live::Log { stream, chunk } = live else {
            return (self.feed)(self.task, self.attempt, live);
        };
        let shown = {
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let text = std::mem::take(&mut held[stream as usize]) + &chunk;
            let (shown, used) = hide(text.as_bytes(), &self.secrets, false);
            held[stream as usize] = String::from_utf8_lossy(&text.as_bytes()[used..]).into_owned();
            shown
        };
        self.emit(stream, &shown);
    }

    fn emit(&self, stream: LogStream, text: &str) {
        for chunk in chunks(text) {
            let chunk = chunk.to_owned();
            (self.feed)(self.task, self.attempt, Live::Log { stream, chunk });
        }
    }
}

/// The attempt ended: what the streams held back is not the start of a secret.
impl Drop for Hiding {
    fn drop(&mut self) {
        let held = std::mem::take(
            self.held
                .get_mut()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        for (stream, text) in [LogStream::Stdout, LogStream::Stderr].into_iter().zip(held) {
            let (shown, _) = hide(text.as_bytes(), &self.secrets, true);
            self.emit(stream, &shown);
        }
    }
}

/// `text` with each of `secrets`, longest first, as `[secret]`, from the left, and how much of
/// `text` that is. Without `last`, it stops where the rest of `text` may start a secret.
fn hide(text: &[u8], secrets: &[String], last: bool) -> (String, usize) {
    let mut shown = Vec::with_capacity(text.len());
    let mut at = 0;
    while let Some(rest) = text.get(at..).filter(|rest| !rest.is_empty()) {
        let open =
            |secret: &String| secret.len() > rest.len() && secret.as_bytes().starts_with(rest);
        if !last && secrets.iter().any(open) {
            break;
        }
        match secrets
            .iter()
            .find(|secret| rest.starts_with(secret.as_bytes()))
        {
            Some(secret) => {
                shown.extend_from_slice(b"[secret]");
                at += secret.len();
            }
            None => {
                shown.push(rest[0]);
                at += 1;
            }
        }
    }
    (String::from_utf8_lossy(&shown).into_owned(), at)
}

impl std::fmt::Debug for LiveFeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LiveFeed")
    }
}

/// The text in chunks of whole lines of at most [`CHUNK`] bytes; a longer line is cut at a
/// character boundary.
pub fn chunks(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len();
        if end > CHUNK {
            end = rest.floor_char_boundary(CHUNK);
            if let Some(line) = rest[..end].rfind('\n') {
                end = line + 1;
            }
        }
        found.push(&rest[..end]);
        rest = &rest[end..];
    }
    found
}

/// Reads what a task appended to its output files since the last look, like `tail -f`. The
/// task can write its control folder, so the files are read only through the folder opened
/// at the start and never through a link.
#[derive(Debug)]
pub struct Tail {
    folder: Option<OwnedFd>,
    files: [(LogStream, &'static str, u64, Vec<u8>); 2],
}

impl Tail {
    /// Follows the files `stdout` and `stderr` of a control folder.
    pub fn new(control: &Path) -> Tail {
        Tail {
            folder: folder(control).ok(),
            files: [
                (LogStream::Stdout, "stdout", 0, Vec::new()),
                (LogStream::Stderr, "stderr", 0, Vec::new()),
            ],
        }
    }

    /// Sends the new whole lines; with `last`, all output to the end of the files, in blocks
    /// of a bounded size, and an unfinished last line.
    pub fn read(&mut self, sink: &LiveSink, last: bool) {
        let Some(folder) = &self.folder else { return };
        for (stream, name, offset, partial) in &mut self.files {
            let Ok(mut file) = regular(folder, name, OFlags::RDONLY) else {
                continue;
            };
            loop {
                let mut bytes = Vec::new();
                let read = (file.seek(SeekFrom::Start(*offset)))
                    .and_then(|_| (&mut file).take(READ_LIMIT).read_to_end(&mut bytes));
                let Ok(read) = read else { break };
                *offset += read as u64;
                partial.extend_from_slice(&bytes);
                let end = match partial.iter().rposition(|byte| *byte == b'\n') {
                    _ if last && read == 0 => partial.len(),
                    _ if partial.len() >= CHUNK => whole_characters(partial),
                    Some(line) => line + 1,
                    None => 0,
                };
                if end > 0 {
                    let whole: Vec<u8> = partial.drain(..end).collect();
                    sink.log(*stream, &String::from_utf8_lossy(&whole));
                }
                if !last || read == 0 {
                    break;
                }
            }
        }
    }
}

/// How many bytes of `bytes` hold whole characters: all but the start of a character that
/// the next read completes.
fn whole_characters(bytes: &[u8]) -> usize {
    let tail = bytes.len().saturating_sub(3);
    let start = (tail..bytes.len())
        .rev()
        .find(|at| bytes[*at] & 0xC0 != 0x80);
    match start {
        Some(at) if std::str::from_utf8(&bytes[at..]).is_err() && bytes[at] >= 0xC0 => at,
        _ => bytes.len(),
    }
}

/// Appends `text` to the file `name` of the control folder of an attempt, never through a
/// link that the task left there.
pub fn append(control: &Path, name: &str, text: &str) -> std::io::Result<()> {
    let folder = folder(control)?;
    let mut file = regular(
        &folder,
        name,
        OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
    )?;
    file.write_all(text.as_bytes())
}

/// Opens a folder, which must not be a link.
fn folder(path: &Path) -> std::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::open(path, flags, Mode::empty())?)
}

/// Opens the file `name` of an open folder when it is a regular file and not a link.
fn regular(folder: &OwnedFd, name: &str, flags: OFlags) -> std::io::Result<std::fs::File> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::openat(folder.as_fd(), name, flags, Mode::from_raw_mode(0o644))?;
    let kind = FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode);
    match kind {
        FileType::RegularFile => Ok(std::fs::File::from(file)),
        _ => Err(std::io::Error::other(format!(
            "{name} is not a regular file"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_whole_lines() {
        assert_eq!(chunks("a\nb\n"), ["a\nb\n"]);
        let long = format!("{}\n{}", "x".repeat(CHUNK - 2), "y".repeat(10));
        let split = chunks(&long);
        assert_eq!(split.len(), 2);
        assert!(split[0].ends_with('\n') && split[0].len() == CHUNK - 1);
        let wide = "é".repeat(CHUNK);
        let parts = chunks(&wide);
        assert!(parts.iter().all(|part| part.len() <= CHUNK));
        assert_eq!(parts.concat(), wide);
        assert!(chunks("").is_empty());
    }

    #[test]
    fn splits_wide_characters() {
        // A three-byte character crosses byte 4096 of the text.
        let wide = format!("ab{}", "€".repeat(CHUNK));
        let parts = chunks(&wide);
        assert!(
            parts
                .iter()
                .all(|part| !part.is_empty() && part.len() <= CHUNK)
        );
        assert_eq!(parts.concat(), wide);
    }

    #[test]
    fn tails_split_characters() {
        let folder = tempfile::tempdir().unwrap();
        let (sink, seen) = recorder();
        let mut tail = Tail::new(folder.path());
        // The first look ends inside the three bytes of a `€` at byte 4096.
        let text = format!("ab{}", "€".repeat(CHUNK / 3 + 1));
        let bytes = text.as_bytes();
        std::fs::write(folder.path().join("stdout"), &bytes[..CHUNK]).unwrap();
        tail.read(&sink, false);
        std::fs::write(folder.path().join("stdout"), bytes).unwrap();
        tail.read(&sink, true);
        let shown = joined(&seen, LogStream::Stdout);
        assert!(!shown.contains('\u{FFFD}'));
        assert_eq!(shown, text);
    }

    #[test]
    fn drains_final_burst() {
        let folder = tempfile::tempdir().unwrap();
        let (sink, seen) = recorder();
        let mut tail = Tail::new(folder.path());
        let line = format!("{}\n", "x".repeat(99));
        let text = line.repeat((3 * READ_LIMIT as usize) / line.len());
        std::fs::write(folder.path().join("stderr"), &text).unwrap();
        tail.read(&sink, true);
        assert_eq!(joined(&seen, LogStream::Stderr).len(), text.len());
    }

    #[test]
    fn skips_linked_output() {
        let folder = tempfile::tempdir().unwrap();
        let outside = folder.path().join("secret.txt");
        std::fs::write(&outside, "host secret\n").unwrap();
        let control = folder.path().join("control");
        std::fs::create_dir(&control).unwrap();
        std::os::unix::fs::symlink(&outside, control.join("stdout")).unwrap();
        std::os::unix::fs::symlink(&outside, control.join("stderr")).unwrap();
        let (sink, seen) = recorder();
        let mut tail = Tail::new(&control);
        tail.read(&sink, true);
        assert!(seen.lock().unwrap().is_empty());
        let error = append(&control, "stderr", "note\n").expect_err("a link is refused");
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host secret\n");
        // A linked control folder is refused as well.
        let linked = folder.path().join("linked");
        std::os::unix::fs::symlink(&control, &linked).unwrap();
        std::fs::remove_file(control.join("stderr")).unwrap();
        assert!(append(&linked, "stderr", "note\n").is_err());
        assert!(!control.join("stderr").exists());
        append(&control, "stderr", "note\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(control.join("stderr")).unwrap(),
            "note\n"
        );
    }

    #[test]
    fn hides_split_secrets() {
        let (sink, seen) = recorder();
        let feed = LiveFeed::new(move |_, _, live| sink.send(live))
            .hiding(["hunter2-secret".to_owned(), String::new()]);
        let attempt = feed.sink(1, 1);
        // The secret starts 4 bytes before the end of the first chunk of 4096 bytes.
        let text = format!("{}hunter2-secret tail\n", "x".repeat(CHUNK - 4));
        attempt.log(LogStream::Stdout, &text);
        attempt.log(LogStream::Stderr, "ends with hunt");
        drop(attempt);
        let shown = joined(&seen, LogStream::Stdout);
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(shown, format!("{}[secret] tail\n", "x".repeat(CHUNK - 4)));
        assert_eq!(joined(&seen, LogStream::Stderr), "ends with hunt");
    }

    #[test]
    fn hides_longest_first() {
        let secrets = ["abcd".to_owned(), "ab".to_owned()];
        assert_eq!(
            hide(b"xabcdyab", &secrets, true),
            ("x[secret]y[secret]".into(), 8)
        );
        assert_eq!(hide(b"xab", &secrets, false), ("x".into(), 1));
        assert_eq!(hide(b"xabe", &secrets, false), ("x[secret]e".into(), 4));
    }

    type Seen = Arc<Mutex<Vec<Live>>>;

    fn recorder() -> (LiveSink, Seen) {
        let seen = Seen::default();
        let kept = seen.clone();
        (
            LiveSink::new(move |live| kept.lock().unwrap().push(live)),
            seen,
        )
    }

    fn joined(seen: &Seen, wanted: LogStream) -> String {
        (seen.lock().unwrap().iter())
            .filter_map(|live| match live {
                Live::Log { stream, chunk } if *stream == wanted => Some(chunk.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tails_new_lines() {
        let folder = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        let sink = LiveSink::new(move |live| kept.lock().unwrap().push(live));
        let mut tail = Tail::new(folder.path());
        tail.read(&sink, false);
        std::fs::write(folder.path().join("stdout"), "first\nsec").unwrap();
        tail.read(&sink, false);
        std::fs::write(folder.path().join("stdout"), "first\nsecond\n").unwrap();
        std::fs::write(folder.path().join("stderr"), "warn").unwrap();
        tail.read(&sink, false);
        tail.read(&sink, true);
        let log = |stream, chunk: &str| Live::Log {
            stream,
            chunk: chunk.into(),
        };
        assert_eq!(
            *seen.lock().unwrap(),
            [
                log(LogStream::Stdout, "first\n"),
                log(LogStream::Stdout, "second\n"),
                log(LogStream::Stderr, "warn"),
            ]
        );
    }

    #[test]
    fn drops_when_full() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (seen, heard) = std::sync::mpsc::channel();
        let feed = LiveFeed::bounded(1, move |task, attempt, _| {
            let _ = wait.recv();
            let _ = seen.send((task, attempt));
        })
        .unwrap();
        let sink = feed.sink(3, 2);
        let usage = Live::Usage {
            cpu_percent: None,
            memory_bytes: Some(1),
        };
        // The thread takes the first, the queue holds the second, the rest are dropped.
        for _ in 0..50 {
            sink.send(usage.clone());
        }
        drop((feed, sink));
        for _ in 0..50 {
            let _ = release.send(());
        }
        let delivered: Vec<_> = heard.iter().collect();
        assert!((1..=2).contains(&delivered.len()), "{delivered:?}");
        assert!(delivered.iter().all(|pair| *pair == (3, 2)));
    }
}
