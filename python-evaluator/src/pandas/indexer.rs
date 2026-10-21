//! `loc`, `iloc`, `at` and `iat`: selection of rows and columns by label or by position.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::frame::{DataFrame, FrameData};
use super::labels::Labels;
use super::series::{Series, SeriesData};
use super::{is_list_like, is_mask, positions_of, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

/// Rows or columns chosen by a key: their positions, whether the key named exactly one of
/// them (a scalar), and the labels of the result.
pub struct Selection {
    pub positions: Vec<usize>,
    pub scalar: bool,
    pub labels: Labels,
}

impl Selection {
    fn all(labels: &Labels) -> Selection {
        Selection {
            positions: (0..labels.len()).collect(),
            scalar: false,
            labels: labels.clone(),
        }
    }

    fn many(labels: &Labels, positions: Vec<usize>) -> Selection {
        Selection {
            labels: labels.take(&positions),
            positions,
            scalar: false,
        }
    }
}

/// Selects by label as `loc` does: one label, a list of labels, a mask, a slice of labels,
/// or for several levels a tuple, whose first values alone select all rows below them.
pub fn select(
    interpreter: &mut Interpreter,
    labels: &Labels,
    key: &Value,
) -> Result<Selection, Error> {
    if let Value::Slice(parts) = key.plain() {
        if parts.iter().all(Value::is_none) {
            return Ok(Selection::all(labels));
        }
        let step = if parts[2].is_none() {
            1
        } else {
            parts[2]
                .as_int()
                .ok_or_else(|| Error::type_error("slice step must be an integer"))?
        };
        if step == 0 {
            return Err(Error::value_error("slice step cannot be zero"));
        }
        if labels.is_empty() {
            return Ok(Selection::many(labels, Vec::new()));
        }
        let endpoint = |value: &Value, start: bool| -> Result<i64, Error> {
            if value.is_none() {
                return Ok(if start == (step > 0) {
                    0
                } else {
                    labels.len() as i64 - 1
                });
            }
            let found = labels.find(value);
            let position = if start == (step > 0) {
                found.first()
            } else {
                found.last()
            };
            position
                .copied()
                .map(|position| position as i64)
                .ok_or_else(|| Error::key_error(value.clone()))
        };
        let mut position = endpoint(&parts[0], true)?;
        let stop = endpoint(&parts[1], false)?;
        let mut positions = Vec::new();
        while if step > 0 {
            position <= stop
        } else {
            position >= stop
        } {
            positions.push(position as usize);
            let Some(next) = position.checked_add(step) else {
                break;
            };
            position = next;
        }
        return Ok(Selection::many(labels, positions));
    }
    if let Some(positions) = super::sequence::aligned_mask(key, labels)? {
        return Ok(Selection::many(labels, positions));
    }
    if is_mask(interpreter, key, labels.len())? {
        let positions = positions_of(interpreter, key, labels.len())?;
        return Ok(Selection::many(labels, positions));
    }
    if is_list_like(key) && !matches!(key.plain(), Value::Tuple(_)) {
        let mut positions = Vec::new();
        for label in seq_values(interpreter, key)? {
            // A value of the first level selects all rows below it, with all levels.
            let found = if labels.is_multi() && !matches!(label.plain(), Value::Tuple(_)) {
                labels.find_prefix(std::slice::from_ref(&label))
            } else {
                labels.find(&label)
            };
            if found.is_empty() {
                return Err(Error::key_error(label));
            }
            positions.extend(found);
        }
        return Ok(Selection::many(labels, positions));
    }
    if labels.is_multi() {
        let parts: Vec<Value> = match key.plain() {
            Value::Tuple(parts) => parts.to_vec(),
            other => vec![other.clone()],
        };
        if parts.len() >= labels.levels() {
            let positions = labels.find(key);
            return match positions.len() {
                0 => Err(Error::key_error(key.clone())),
                1 => Ok(Selection {
                    labels: labels.take(&positions),
                    positions,
                    scalar: true,
                }),
                _ => Ok(Selection::many(labels, positions)),
            };
        }
        let positions = labels.find_prefix(&parts);
        if positions.is_empty() {
            return Err(Error::key_error(key.clone()));
        }
        return Ok(Selection {
            labels: labels.take(&positions).drop_levels(parts.len()),
            positions,
            scalar: false,
        });
    }
    let positions = labels.find(key);
    match positions.len() {
        0 => Err(Error::key_error(key.clone())),
        1 => Ok(Selection {
            labels: labels.take(&positions),
            positions,
            scalar: true,
        }),
        _ => Ok(Selection::many(labels, positions)),
    }
}

/// Selects by position as `iloc` does: one position, a list of them, a slice or a mask.
fn select_positions(
    interpreter: &mut Interpreter,
    labels: &Labels,
    key: &Value,
) -> Result<Selection, Error> {
    if let Value::Int(_) = key.plain() {
        let position = crate::interpreter::call::normalize_index(
            key,
            labels.len() as i64,
            "single positional indexer is out-of-bounds: index",
        )?;
        return Ok(Selection {
            labels: labels.take(&[position]),
            positions: vec![position],
            scalar: true,
        });
    }
    let positions = positions_of(interpreter, key, labels.len())?;
    Ok(Selection::many(labels, positions))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Label,
    Position,
}

/// The `loc`, `iloc`, `at` or `iat` of a series or table.
pub struct Indexer {
    target: Value,
    kind: Kind,
}

impl Indexer {
    pub fn value(target: &Value, name: &str) -> Value {
        Value::native(Indexer {
            target: target.clone(),
            kind: if name == "iloc" || name == "iat" {
                Kind::Position
            } else {
                Kind::Label
            },
        })
    }

    fn select(
        &self,
        interpreter: &mut Interpreter,
        labels: &Labels,
        key: &Value,
    ) -> Result<Selection, Error> {
        let key = if self.kind == Kind::Label && interpreter.callable(key) {
            interpreter.call(key, Args::new(vec![self.target.clone()]))?
        } else {
            key.clone()
        };
        match self.kind {
            Kind::Label => select(interpreter, labels, &key),
            Kind::Position => select_positions(interpreter, labels, &key),
        }
    }

    /// The row and column keys of a key for a table.
    fn split(
        &self,
        interpreter: &mut Interpreter,
        data: &FrameData,
        key: &Value,
    ) -> Result<(Value, Option<Value>), Error> {
        let Value::Tuple(parts) = key.plain() else {
            return Ok((key.clone(), None));
        };
        let index = data.index();
        // A tuple that names rows of a MultiIndex selects rows only.
        if self.kind == Kind::Label
            && index.is_multi()
            && parts.len() <= index.levels()
            && !parts
                .iter()
                .any(|part| matches!(part.plain(), Value::Slice(_)) || is_list_like(part))
        {
            match select(interpreter, &index, key) {
                Ok(_) => return Ok((key.clone(), None)),
                Err(error) if error.is("KeyError") && parts.len() > 2 => return Err(error),
                Err(error) if error.is("KeyError") => {}
                Err(error) => return Err(error),
            }
        }
        match parts.len() {
            2 => Ok((parts[0].clone(), Some(parts[1].clone()))),
            _ => Err(Error::new("IndexingError", "Too many indexers")),
        }
    }

    fn get_series(
        &self,
        interpreter: &mut Interpreter,
        data: SeriesData,
        key: &Value,
    ) -> Result<Value, Error> {
        let selection = self.select(interpreter, &data.index, key)?;
        if selection.scalar {
            return Ok(data.values[selection.positions[0]].clone());
        }
        let mut taken = data.take(&selection.positions);
        taken.index = selection.labels;
        Ok(Series::from_data(taken))
    }

    fn get_frame(
        &self,
        interpreter: &mut Interpreter,
        data: FrameData,
        key: &Value,
    ) -> Result<Value, Error> {
        let (rows, columns) = self.split(interpreter, &data, key)?;
        let index = data.index();
        let rows = self.select(interpreter, &index, &rows)?;
        let columns = match columns {
            Some(columns) => self.select(interpreter, &data.columns, &columns)?,
            None => Selection::all(&data.columns),
        };
        Ok(match (rows.scalar, columns.scalar) {
            (true, true) => data.data[columns.positions[0]][rows.positions[0]].clone(),
            (true, false) => {
                let mut row = data.row(rows.positions[0], &columns.positions);
                row.index = columns.labels;
                Series::from_data(row)
            }
            (false, true) => {
                let column = columns.positions[0];
                Series::value(
                    rows.positions
                        .iter()
                        .map(|row| data.data[column][*row].clone())
                        .collect(),
                    rows.labels,
                    data.columns.values[column].clone(),
                )
            }
            (false, false) => {
                let mut taken = data
                    .take_columns(&columns.positions)
                    .take_rows(&rows.positions);
                taken.columns = columns.labels;
                *taken.index.borrow_mut() = rows.labels;
                DataFrame::from_data(taken)
            }
        })
    }

    fn set_series(
        &self,
        interpreter: &mut Interpreter,
        series: &Series,
        key: &Value,
        value: Value,
    ) -> Result<(), Error> {
        let index = series.data.borrow().index.clone();
        let positions = match self.select(interpreter, &index, key) {
            Ok(selection) => selection.positions,
            Err(error)
                if error.is("KeyError") && self.kind == Kind::Label && !is_list_like(key) =>
            {
                let mut data = series.data.borrow_mut();
                data.index.values.push(key.clone());
                data.values.push(value);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let values = spread(interpreter, &value, positions.len())?;
        let mut data = series.data.borrow_mut();
        for (position, value) in positions.into_iter().zip(values) {
            data.values[position] = value;
        }
        Ok(())
    }

    fn set_frame(
        &self,
        interpreter: &mut Interpreter,
        frame: &DataFrame,
        key: &Value,
        value: Value,
    ) -> Result<(), Error> {
        let data = frame.data();
        let (rows_key, columns_key) = self.split(interpreter, &data, key)?;
        let index = data.index();
        let rows = match self.select(interpreter, &index, &rows_key) {
            Ok(selection) => selection.positions,
            Err(error)
                if error.is("KeyError") && self.kind == Kind::Label && !is_list_like(&rows_key) =>
            {
                // A new row, missing in every column until set.
                let mut data = frame.data.borrow_mut();
                data.index.borrow_mut().values.push(rows_key.clone());
                for column in &mut data.data {
                    column.push(super::nan());
                }
                vec![data.rows() - 1]
            }
            Err(error) => return Err(error),
        };
        let columns = match columns_key {
            None => data.all_columns(),
            Some(columns_key) => {
                let labels = frame.data.borrow().columns.clone();
                match self.select(interpreter, &labels, &columns_key) {
                    Ok(selection) => selection.positions,
                    Err(error)
                        if error.is("KeyError")
                            && self.kind == Kind::Label
                            && !is_list_like(&columns_key) =>
                    {
                        let mut data = frame.data.borrow_mut();
                        let rows = data.rows();
                        data.set_column(columns_key.clone(), vec![super::nan(); rows]);
                        vec![data.columns.len() - 1]
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        // One row or one column takes a sequence of its length; a block takes one value
        // per column for all its rows.
        let single = rows.len() == 1 || columns.len() == 1;
        let count = if rows.len() == 1 {
            columns.len()
        } else if single {
            rows.len()
        } else {
            columns.len()
        };
        let values = spread(interpreter, &value, count)?;
        let mut data = frame.data.borrow_mut();
        if single {
            for (item, value) in values.into_iter().enumerate() {
                if rows.len() == 1 {
                    data.data[columns[item]][rows[0]] = value;
                } else {
                    data.data[columns[0]][rows[item]] = value;
                }
            }
        } else {
            for (column, value) in columns.iter().zip(values) {
                for row in &rows {
                    data.data[*column][*row] = value.clone();
                }
            }
        }
        Ok(())
    }
}

/// The value for each of `count` cells: the items of a sequence of that length, else the
/// value itself for each.
fn spread(interpreter: &mut Interpreter, value: &Value, count: usize) -> Result<Vec<Value>, Error> {
    if is_list_like(value) {
        let values = seq_values(interpreter, value)?;
        if values.len() == count {
            return Ok(values);
        }
        if count != 1 {
            return Err(Error::value_error(format!(
                "Must have equal len keys and value when setting with an iterable ({} values for {count} cells)",
                values.len()
            )));
        }
    }
    Ok(vec![value.clone(); count])
}

impl Native for Indexer {
    fn type_name(&self) -> &str {
        match self.kind {
            Kind::Label => "_LocIndexer",
            Kind::Position => "_iLocIndexer",
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        if let Some(series) = self.target.as_native::<Series>() {
            let data = series.data();
            return self.get_series(interpreter, data, key).map(Some);
        }
        if let Some(frame) = self.target.as_native::<DataFrame>() {
            let data = frame.data();
            return self.get_frame(interpreter, data, key).map(Some);
        }
        Ok(None)
    }

    fn set_item(
        &self,
        interpreter: &mut Interpreter,
        key: &Value,
        value: Value,
    ) -> Result<bool, Error> {
        if let Some(series) = self.target.as_native::<Series>() {
            self.set_series(interpreter, series, key, value)?;
            return Ok(true);
        }
        if let Some(frame) = self.target.as_native::<DataFrame>() {
            self.set_frame(interpreter, frame, key, value)?;
            return Ok(true);
        }
        Ok(false)
    }
}
