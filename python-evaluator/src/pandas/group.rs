//! `DataFrame.groupby`: the rows of a table in groups by the values of columns or index
//! levels, and aggregations over each group.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use super::frame::{DataFrame, FrameData};
use super::index::IndexObject;
use super::labels::Labels;
use super::series::{Series, SeriesData};
use super::{is_list_like, is_missing, method, same_label, seq_values};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Dict, Key, Native, Value};

/// The groups of a table: the value of the grouping key of each, and its row positions.
pub struct GroupBy {
    frame: FrameData,
    names: Vec<Value>,
    groups: Vec<(Value, Vec<usize>)>,
    /// The column a `groupby(...)["col"]` selects, which makes results series.
    column: Option<Value>,
    as_index: bool,
}

/// `groupby(by=None, level=None, sort=True, as_index=True, dropna=True)`.
pub fn groupby(
    interpreter: &mut Interpreter,
    frame: FrameData,
    mut args: Args,
) -> Result<Value, Error> {
    let by = args.take(0, "by").filter(|by| !by.is_none());
    let level = args.take_keyword("level").filter(|level| !level.is_none());
    let sort = match args.take_keyword("sort") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    let as_index = match args.take_keyword("as_index") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    let dropna = match args.take_keyword("dropna") {
        Some(value) => interpreter.truthy(&value)?,
        None => true,
    };
    let index = frame.index();
    let mut keys: Vec<Vec<Value>> = Vec::new();
    let mut names = Vec::new();
    if let Some(level) = level {
        let levels = if is_list_like(&level) {
            seq_values(interpreter, &level)?
        } else {
            vec![level]
        };
        for level in levels {
            let position = index
                .level_of(&level)
                .ok_or_else(|| Error::key_error(level.clone()))?;
            keys.push(index.level(position));
            names.push(index.names[position].clone());
        }
    }
    if let Some(by) = by {
        let items = if is_list_like(&by) && by.as_native::<Series>().is_none() {
            seq_values(interpreter, &by)?
        } else {
            vec![by]
        };
        for item in items {
            if let Some(series) = item.as_native::<Series>() {
                let series = series.data();
                keys.push(series.values);
                names.push(series.name);
            } else if let Some(position) = frame.column_position(&item) {
                keys.push(frame.data[position].clone());
                names.push(item);
            } else if let Some(level) = index
                .level_of(&item)
                .filter(|_| !matches!(item.plain(), Value::Int(_)))
            {
                keys.push(index.level(level));
                names.push(item);
            } else if interpreter.callable(&item) {
                let mut values = Vec::with_capacity(index.len());
                for label in &index.values {
                    values.push(interpreter.call(&item, Args::new(vec![label.clone()]))?);
                }
                keys.push(values);
                names.push(Value::None);
            } else {
                return Err(Error::key_error(item));
            }
        }
    }
    if keys.is_empty() {
        return Err(Error::type_error(
            "You have to supply one of 'by' and 'level'",
        ));
    }
    let mut groups: Vec<(Value, Vec<usize>)> = Vec::new();
    for row in 0..frame.rows() {
        let parts: Vec<Value> = keys.iter().map(|key| key[row].clone()).collect();
        if dropna && parts.iter().any(is_missing) {
            continue;
        }
        let key = if parts.len() == 1 {
            parts[0].clone()
        } else {
            Value::tuple(parts)
        };
        match groups.iter_mut().find(|(known, _)| same_label(known, &key)) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((key, vec![row])),
        }
    }
    if sort {
        let order = super::sorted_positions(
            interpreter,
            &groups
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            false,
        )?;
        groups = order
            .into_iter()
            .map(|position| groups[position].clone())
            .collect();
    }
    Ok(Value::native(GroupBy {
        frame,
        names,
        groups,
        column: None,
        as_index,
    }))
}

impl GroupBy {
    fn keys(&self) -> Labels {
        let keys: Vec<Value> = self.groups.iter().map(|(key, _)| key.clone()).collect();
        if self.names.len() == 1 {
            return Labels::single(keys, self.names[0].clone());
        }
        Labels {
            values: keys,
            names: self.names.clone(),
        }
    }

    /// The columns that aggregations work on: the selected one, or all but the keys.
    fn value_columns(&self) -> Vec<usize> {
        match &self.column {
            Some(column) => self.frame.column_position(column).into_iter().collect(),
            None => self
                .frame
                .all_columns()
                .into_iter()
                .filter(|position| {
                    !self
                        .names
                        .iter()
                        .any(|name| same_label(name, &self.frame.columns.values[*position]))
                })
                .collect(),
        }
    }

    /// One value per group and column from a function of the values; a series for a
    /// selected column, else a table.
    fn aggregate(
        &self,
        interpreter: &mut Interpreter,
        function: &mut dyn FnMut(&mut Interpreter, SeriesData) -> Result<Value, Error>,
    ) -> Result<Value, Error> {
        let columns = self.value_columns();
        let mut data: Vec<Vec<Value>> = vec![Vec::with_capacity(self.groups.len()); columns.len()];
        for (_, rows) in &self.groups {
            for (slot, column) in columns.iter().enumerate() {
                data[slot].push(function(
                    interpreter,
                    self.frame.column(*column).take(rows),
                )?);
            }
        }
        let keys = self.keys();
        if self.column.is_some() {
            let name = self.column.clone().unwrap_or(Value::None);
            return Ok(Series::value(
                data.into_iter().next().unwrap_or_default(),
                keys,
                name,
            ));
        }
        let mut result = FrameData::new(self.frame.columns.take(&columns), data, keys);
        if !self.as_index {
            let mut args = Args::default();
            result = super::reshape::reset_index(interpreter, result, &mut args)?;
        }
        Ok(DataFrame::from_data(result))
    }

    /// `transform(func)` of a selected column: the result of each group for each of its rows,
    /// in the order of the table; rows in no group get `nan`.
    fn transform(&self, interpreter: &mut Interpreter, function: &Value) -> Result<Value, Error> {
        let Some(label) = self.column.clone() else {
            return Err(Error::unsupported("transform of a grouped table"));
        };
        let mut results = Vec::with_capacity(self.groups.len());
        let position = self
            .frame
            .column_position(&label)
            .ok_or_else(|| Error::key_error(label.clone()))?;
        for (_, rows) in &self.groups {
            let series = self.frame.column(position).take(rows);
            let result = match function.as_str() {
                Some(name) => {
                    let grouped = GroupBy {
                        frame: self.frame.take_rows(rows),
                        names: self.names.clone(),
                        groups: vec![(Value::None, (0..rows.len()).collect())],
                        column: Some(label.clone()),
                        as_index: true,
                    };
                    let aggregated = grouped.named(interpreter, name)?;
                    aggregated
                        .as_native::<Series>()
                        .and_then(|series| series.data().values.first().cloned())
                        .unwrap_or_else(super::nan)
                }
                None => interpreter.call(function, Args::new(vec![Series::from_data(series)]))?,
            };
            results.push(result);
        }
        let mut values = vec![super::nan(); self.frame.rows()];
        for ((_, rows), result) in self.groups.iter().zip(results) {
            // A result per row, or one for the whole group.
            let per_row = result
                .as_native::<Series>()
                .map(|series| series.data().values)
                .filter(|items| items.len() == rows.len());
            for (index, row) in rows.iter().enumerate() {
                values[*row] = match &per_row {
                    Some(items) => items[index].clone(),
                    None => result.clone(),
                };
            }
        }
        Ok(Series::value(values, self.frame.index(), label))
    }

    fn named(&self, interpreter: &mut Interpreter, name: &str) -> Result<Value, Error> {
        let name = name.to_owned();
        self.aggregate(interpreter, &mut |interpreter, data| {
            let present: Vec<Value> = data
                .values
                .iter()
                .filter(|value| !is_missing(value))
                .cloned()
                .collect();
            Ok(match name.as_str() {
                "first" => present.first().cloned().unwrap_or_else(super::nan),
                "last" => present.last().cloned().unwrap_or_else(super::nan),
                "count" => Value::Int(present.len() as i64),
                "nunique" => Value::Int(super::distinct(&present).len() as i64),
                "sum" => super::sum(interpreter, &present)?,
                "max" => super::extreme(interpreter, &present, true)?,
                "min" => super::extreme(interpreter, &present, false)?,
                "unique" => super::Array::value(super::distinct(&data.values)),
                "list" => Value::list(data.values),
                _ => {
                    return Err(Error::unsupported(format!(
                        "the groupby aggregation {name}"
                    )));
                }
            })
        })
    }
}

impl Native for GroupBy {
    fn type_name(&self) -> &str {
        if self.column.is_some() {
            "SeriesGroupBy"
        } else {
            "DataFrameGroupBy"
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        interpreter: &mut Interpreter,
        this: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(match name {
            "ngroups" => Value::Int(self.groups.len() as i64),
            "groups" | "indices" => {
                let index = self.frame.index();
                let mut dict = Dict::default();
                for (key, rows) in &self.groups {
                    let value = if name == "groups" {
                        IndexObject::value(index.take(rows))
                    } else {
                        super::Array::value(
                            rows.iter().map(|row| Value::Int(*row as i64)).collect(),
                        )
                    };
                    dict.entries.insert(Key::new(key.clone())?, value);
                }
                Value::dict(dict)
            }
            "get_group" => method::<GroupBy>(this, name, |_, groups, args| {
                let key = args.get(0).cloned().unwrap_or(Value::None);
                let rows = groups
                    .groups
                    .iter()
                    .find(|(known, _)| same_label(known, &key))
                    .map(|(_, rows)| rows.clone())
                    .ok_or_else(|| Error::key_error(key.clone()))?;
                Ok(DataFrame::from_data(groups.frame.take_rows(&rows)))
            }),
            "size" => method::<GroupBy>(this, name, |_, groups, _| {
                let counts = groups
                    .groups
                    .iter()
                    .map(|(_, rows)| Value::Int(rows.len() as i64))
                    .collect();
                Ok(Series::value(counts, groups.keys(), Value::None))
            }),
            "first" | "last" | "count" | "nunique" | "sum" | "max" | "min" | "unique" => {
                let which = name.to_owned();
                method::<GroupBy>(this, name, move |interpreter, groups, _| {
                    groups.named(interpreter, &which)
                })
            }
            "agg" | "aggregate" => method::<GroupBy>(this, name, |interpreter, groups, args| {
                let function = args.get(0).cloned().unwrap_or(Value::None);
                if let Some(name) = function.as_str() {
                    return groups.named(interpreter, name);
                }
                if let Value::Class(class) = function.plain()
                    && &*class.name == "list"
                {
                    return groups.named(interpreter, "list");
                }
                groups.aggregate(interpreter, &mut |interpreter, series| {
                    interpreter.call(&function, Args::new(vec![Series::from_data(series)]))
                })
            }),
            "apply" => method::<GroupBy>(this, name, |interpreter, groups, args| {
                let function = args.get(0).cloned().unwrap_or(Value::None);
                if groups.column.is_some() {
                    return groups.aggregate(interpreter, &mut |interpreter, series| {
                        interpreter.call(&function, Args::new(vec![Series::from_data(series)]))
                    });
                }
                let mut results = Vec::with_capacity(groups.groups.len());
                for (_, rows) in &groups.groups {
                    let frame = DataFrame::from_data(groups.frame.take_rows(rows));
                    results.push(interpreter.call(&function, Args::new(vec![frame]))?);
                }
                Ok(Series::value(results, groups.keys(), Value::None))
            }),
            "transform" => method::<GroupBy>(this, name, |interpreter, groups, args| {
                let function = args.get(0).cloned().unwrap_or(Value::None);
                groups.transform(interpreter, &function)
            }),
            // A column by its name, as `groups.column` selects it.
            _ if self.column.is_none()
                && self.frame.column_position(&Value::str(name)).is_some() =>
            {
                return self.get_item(interpreter, this, &Value::str(name));
            }
            _ => return Ok(None),
        }))
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        if is_list_like(key) {
            let wanted = seq_values(interpreter, key)?;
            let mut keep: Vec<usize> = Vec::new();
            for (position, label) in self.frame.columns.values.iter().enumerate() {
                if wanted.iter().any(|item| same_label(item, label))
                    || self.names.iter().any(|name| same_label(name, label))
                {
                    keep.push(position);
                }
            }
            return Ok(Some(Value::native(GroupBy {
                frame: self.frame.take_columns(&keep),
                names: self.names.clone(),
                groups: self.groups.clone(),
                column: None,
                as_index: self.as_index,
            })));
        }
        if self.frame.column_position(key).is_none() {
            return Err(Error::key_error(key.clone()));
        }
        Ok(Some(Value::native(GroupBy {
            frame: self.frame.clone(),
            names: self.names.clone(),
            groups: self.groups.clone(),
            column: Some(key.clone()),
            as_index: self.as_index,
        })))
    }

    fn iterate(&self, _: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        Ok(Some(
            self.groups
                .iter()
                .map(|(key, rows)| {
                    let group = match &self.column {
                        Some(column) => {
                            let position = self.frame.column_position(column).unwrap_or(0);
                            Series::from_data(self.frame.column(position).take(rows))
                        }
                        None => DataFrame::from_data(self.frame.take_rows(rows)),
                    };
                    Value::tuple(vec![key.clone(), group])
                })
                .collect(),
        ))
    }

    fn length(&self) -> Option<usize> {
        Some(self.groups.len())
    }
}
