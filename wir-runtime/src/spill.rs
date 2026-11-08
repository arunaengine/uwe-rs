//! A stream log in a file: each batch is written at the end in one write, and items are read
//! back by their offset, so long streams keep their history without holding it in memory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::{Value as Json, json};
use workflow_graph::DriverError;

use crate::streams::{StreamId, StreamItem, StreamLog};

/// A log in one file of JSON lines. A batch that the file does not take is cut off again.
pub struct FileLog {
    file: File,
    length: u64,
    /// Where each item of each stream starts in the file, and its length.
    places: HashMap<StreamId, Vec<(u64, usize)>>,
}

impl FileLog {
    /// Makes the log at `path`, which must not exist yet.
    pub fn create(path: &Path) -> Result<FileLog, DriverError> {
        let file = (File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path))
        .map_err(|error| DriverError::new(format!("cannot make {}: {error}", path.display())))?;
        Ok(FileLog::with(file))
    }

    fn with(file: File) -> FileLog {
        FileLog {
            file,
            length: 0,
            places: HashMap::new(),
        }
    }
}

impl StreamLog for FileLog {
    fn append(&mut self, batch: &[(StreamId, StreamItem)]) -> Result<(), DriverError> {
        let mut text = Vec::new();
        let mut places = Vec::new();
        for (stream, item) in batch {
            let start = text.len();
            let line = json!({"event": item.event, "value": item.value});
            serde_json::to_writer(&mut text, &line)
                .map_err(|error| DriverError::new(error.to_string()))?;
            places.push((stream, self.length + start as u64, text.len() - start));
            text.push(b'\n');
        }
        let written = (self.file.seek(SeekFrom::Start(self.length)))
            .and_then(|_| self.file.write_all(&text))
            .and_then(|()| self.file.flush());
        if let Err(error) = written {
            // A torn batch must not stay in the file.
            let _ = self.file.set_len(self.length);
            return Err(DriverError::new(format!(
                "the stream log took no batch: {error}"
            )));
        }
        self.length += text.len() as u64;
        for (stream, start, length) in places {
            self.places
                .entry(stream.clone())
                .or_default()
                .push((start, length));
        }
        Ok(())
    }

    fn item(&self, stream: &StreamId, sequence: u64) -> Result<Option<StreamItem>, DriverError> {
        let place = self
            .places
            .get(stream)
            .and_then(|places| places.get(sequence as usize));
        let Some((start, length)) = place.copied() else {
            return Ok(None);
        };
        let failed = |error: std::io::Error| {
            DriverError::new(format!("cannot read the stream log: {error}"))
        };
        let mut file = self.file.try_clone().map_err(failed)?;
        let mut text = vec![0; length];
        file.seek(SeekFrom::Start(start)).map_err(failed)?;
        file.read_exact(&mut text).map_err(failed)?;
        let line: Json =
            serde_json::from_slice(&text).map_err(|error| DriverError::new(error.to_string()))?;
        Ok(Some(StreamItem {
            sequence,
            event: line["event"].as_u64().unwrap_or_default(),
            value: line["value"].clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wir::Subscription;

    use super::*;
    use crate::streams::{MemoryLog, Read, ReaderId, StreamBatch, StreamEnd, Streams};

    /// Writes three batches and reads them with a late reader.
    fn replay(log: Box<dyn StreamLog>) -> Vec<(u64, Json)> {
        let stream = StreamId("a#out".into());
        let mut streams = Streams::new(log);
        streams.open(stream.clone(), "a");
        for values in [
            vec![json!({"n": 1}), json!(null)],
            vec![json!("two\nlines")],
            vec![json!([3])],
        ] {
            let items = values
                .into_iter()
                .map(|value| (stream.clone(), value))
                .collect();
            streams
                .commit(StreamBatch {
                    items,
                    ..StreamBatch::default()
                })
                .unwrap();
        }
        let ends = vec![(stream.clone(), StreamEnd::Close)];
        streams
            .commit(StreamBatch {
                ends,
                ..StreamBatch::default()
            })
            .unwrap();
        let late = ReaderId {
            owner: "late".into(),
            input: "source".into(),
        };
        streams
            .subscribe(&stream, late.clone(), Subscription::All)
            .unwrap();
        let mut seen = Vec::new();
        while let Read::Item(item) = streams.peek(&late).unwrap() {
            seen.push((item.event, item.value));
            streams.consume(std::slice::from_ref(&late)).unwrap();
        }
        assert!(matches!(streams.peek(&late).unwrap(), Read::End(_)));
        seen
    }

    #[test]
    fn stream_spill() {
        let folder = tempfile::tempdir().unwrap();
        let log = FileLog::create(&folder.path().join("streams.jsonl")).unwrap();
        let spilled = replay(Box::new(log));
        assert_eq!(
            spilled,
            replay(Box::new(MemoryLog::default())),
            "the file replays as memory does"
        );
        assert_eq!(spilled[2], (2, json!("two\nlines")));
    }

    #[test]
    fn spill_failure() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("streams.jsonl");
        std::fs::write(&path, "").unwrap();
        // A file opened for reading takes no write.
        let mut log = FileLog::with(File::open(&path).unwrap());
        let stream = StreamId("a#out".into());
        let item = StreamItem {
            sequence: 0,
            event: 1,
            value: json!(1),
        };
        assert!(log.append(&[(stream.clone(), item)]).is_err());
        assert_eq!(
            log.item(&stream, 0).unwrap(),
            None,
            "a refused batch leaves no item"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(
            FileLog::create(&path).is_err(),
            "a log never takes over a file"
        );
    }
}
