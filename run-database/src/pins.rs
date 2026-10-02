//! Finds image digests from earlier real runs for simulated cache decisions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use crate::{Database, DatabaseError};

impl Database {
    /// For each image that tasks named, the digest that the latest real run pinned it to, like
    /// `debian:stable-slim` to `debian@sha256:...`.
    pub fn pins(&self) -> Result<HashMap<String, String>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT json_extract(t.description, '$.image'), a.image
             FROM attempts a
             JOIN tasks t ON t.run = a.run AND t.id = a.task
             JOIN runs r ON r.id = a.run
             WHERE r.simulated = 0 AND a.image LIKE '%@sha256:%'
                AND json_extract(t.description, '$.image') IS NOT NULL
             ORDER BY a.started",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pins = HashMap::new();
        // The latest run comes last and wins.
        for row in rows {
            let (image, pinned) = row?;
            pins.insert(image, pinned);
        }
        Ok(pins)
    }
}
