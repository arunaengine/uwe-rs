//! Pages through remote attempt directories without rescanning history for every admission.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::ReadDir;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct Scan {
    root: PathBuf,
    cursor: Option<ReadDir>,
    finished: Option<Instant>,
}

impl Scan {
    pub fn reset(&mut self) {
        self.cursor = None;
        self.finished = None;
    }

    pub fn complete(&self) -> bool {
        self.finished.is_some()
    }

    pub fn page(&mut self, root: &Path, limit: usize) -> std::io::Result<Vec<PathBuf>> {
        self.page_at(root, limit, Instant::now())
    }

    fn page_at(
        &mut self,
        root: &Path,
        limit: usize,
        now: Instant,
    ) -> std::io::Result<Vec<PathBuf>> {
        if self.root != root {
            self.reset();
            self.root = root.to_owned();
        }
        if self.finished.is_some_and(|finished| {
            now.saturating_duration_since(finished) < Duration::from_secs(1)
        }) {
            return Ok(Vec::new());
        }
        if self.cursor.is_none() {
            self.finished = None;
            self.cursor = match std::fs::read_dir(root) {
                Ok(cursor) => Some(cursor),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.finished = Some(now);
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            };
        }
        let mut paths = Vec::new();
        for _ in 0..limit.max(1) {
            let Some(entry) = self.cursor.as_mut().unwrap().next() else {
                self.cursor = None;
                self.finished = Some(now);
                break;
            };
            let entry = entry?;
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => paths.push(entry.path()),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_history_pages() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..259 {
            std::fs::create_dir(root.path().join(index.to_string())).unwrap();
        }
        let now = Instant::now();
        let mut scan = Scan::default();
        let mut seen = std::collections::BTreeSet::new();
        while !scan.complete() {
            let page = scan.page_at(root.path(), 128, now).unwrap();
            assert!(page.len() <= 128);
            for path in page {
                assert!(seen.insert(path));
            }
        }
        assert_eq!(seen.len(), 259);
        assert!(scan.page_at(root.path(), 128, now).unwrap().is_empty());
        assert_eq!(
            scan.page_at(root.path(), 128, now + Duration::from_secs(1))
                .unwrap()
                .len(),
            128
        );
    }
}
