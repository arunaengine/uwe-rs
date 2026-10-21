//! Represents table labels and indexes, including tuple labels for MultiIndex levels.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::index::IndexObject;
use super::{is_missing, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Value, key_equal};

/// Labels and the names of their levels; a name is `None` for a level without one.
#[derive(Clone)]
pub struct Labels {
    pub values: Vec<Value>,
    pub names: Vec<Value>,
}

impl Labels {
    /// `0, 1, ..., length - 1`, the labels of a table without an index.
    pub fn range(length: usize) -> Labels {
        Labels::single((0..length as i64).map(Value::Int).collect(), Value::None)
    }

    pub fn single(values: Vec<Value>, name: Value) -> Labels {
        Labels {
            values,
            names: vec![name],
        }
    }

    /// Labels with several levels, from one list of values per level.
    pub fn from_levels(levels: Vec<Vec<Value>>, names: Vec<Value>) -> Labels {
        if levels.len() == 1 {
            let values = levels.into_iter().next().unwrap_or_default();
            return Labels::single(values, names.into_iter().next().unwrap_or(Value::None));
        }
        let length = levels.first().map_or(0, Vec::len);
        let values = (0..length)
            .map(|row| Value::tuple(levels.iter().map(|level| level[row].clone()).collect()))
            .collect();
        Labels { values, names }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn levels(&self) -> usize {
        self.names.len().max(1)
    }

    pub fn is_multi(&self) -> bool {
        self.names.len() > 1
    }

    /// Whether the labels are `0, 1, 2, ...`, so that integer keys are positions too.
    pub fn is_integer(&self) -> bool {
        !self.is_multi()
            && self
                .values
                .iter()
                .all(|value| matches!(value.plain(), Value::Int(_)))
    }

    /// The values of one level, for every label.
    pub fn level(&self, level: usize) -> Vec<Value> {
        if !self.is_multi() {
            return self.values.clone();
        }
        self.values
            .iter()
            .map(|label| match label.plain() {
                Value::Tuple(parts) => parts.get(level).cloned().unwrap_or(Value::None),
                other => other.clone(),
            })
            .collect()
    }

    /// The level with that name or number.
    pub fn level_of(&self, key: &Value) -> Option<usize> {
        if let Some(position) = self.names.iter().position(|name| key_equal(name, key)) {
            return Some(position);
        }
        match key.plain() {
            Value::Int(level) if *level >= 0 && (*level as usize) < self.levels() => {
                Some(*level as usize)
            }
            _ => None,
        }
    }

    pub fn take(&self, positions: &[usize]) -> Labels {
        Labels {
            values: positions
                .iter()
                .map(|position| self.values[*position].clone())
                .collect(),
            names: self.names.clone(),
        }
    }

    /// The positions of the labels equal to a key.
    pub fn find(&self, key: &Value) -> Vec<usize> {
        self.values
            .iter()
            .enumerate()
            .filter(|(_, label)| key_equal(label, key))
            .map(|(position, _)| position)
            .collect()
    }

    /// The positions of the labels whose first levels equal the parts of a partial key.
    pub fn find_prefix(&self, parts: &[Value]) -> Vec<usize> {
        self.values
            .iter()
            .enumerate()
            .filter(|(_, label)| match label.plain() {
                Value::Tuple(items) => {
                    items.len() >= parts.len()
                        && items
                            .iter()
                            .zip(parts)
                            .all(|(item, part)| key_equal(item, part))
                }
                _ => false,
            })
            .map(|(position, _)| position)
            .collect()
    }

    /// The labels without their first `count` levels.
    pub fn drop_levels(&self, count: usize) -> Labels {
        let levels: Vec<Vec<Value>> = (count..self.levels())
            .map(|level| self.level(level))
            .collect();
        Labels::from_levels(levels, self.names[count.min(self.names.len())..].to_vec())
    }

    /// Whether no label appears twice.
    pub fn is_unique(&self) -> bool {
        self.values.iter().enumerate().all(|(position, label)| {
            !self.values[..position]
                .iter()
                .any(|other| key_equal(other, label))
        })
    }

    /// The sorted distinct values of each level, as `MultiIndex.levels` gives them.
    pub(super) fn sorted_levels(
        &self,
        interpreter: &mut Interpreter,
    ) -> Result<Vec<Vec<Value>>, Error> {
        let mut levels = Vec::new();
        for level in 0..self.levels() {
            let mut distinct: Vec<Value> = Vec::new();
            for value in self.level(level) {
                if !is_missing(&value) && !distinct.iter().any(|seen| key_equal(seen, &value)) {
                    distinct.push(value);
                }
            }
            levels.push(crate::builtins::sort_values(
                interpreter,
                distinct,
                None,
                false,
            )?);
        }
        Ok(levels)
    }
}

/// The labels of an index given as a list, an index object, a series or an array.
pub fn labels_of(interpreter: &mut Interpreter, value: &Value) -> Result<Labels, Error> {
    if let Some(index) = value.as_native::<IndexObject>() {
        return Ok(index.labels());
    }
    if let Some(series) = value.as_native::<super::series::Series>() {
        let data = series.data.borrow();
        return Ok(Labels::single(data.values.clone(), data.name.clone()));
    }
    Ok(Labels::single(seq_values(interpreter, value)?, Value::None))
}
