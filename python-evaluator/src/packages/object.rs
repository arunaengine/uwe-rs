//! `PackageObject`: the handle of an object that lives in the Python process of the packages.
//! Each use of it is one request to the process.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::rc::Rc;

use serde_json::{Map, Value as Json};

use super::Packages;
use super::protocol::encode;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Args, Native, Value};

pub struct PackageObject {
    packages: Rc<Packages>,
    /// The process the handle belongs to.
    generation: u64,
    handle: u64,
    type_name: String,
    callable: bool,
}

impl PackageObject {
    /// The object of a `{"handle": ...}` value that the process sent.
    pub(super) fn value(
        packages: &Rc<Packages>,
        fields: &Map<String, Json>,
    ) -> Result<Value, Error> {
        let handle = fields.get("handle").and_then(Json::as_u64).ok_or_else(|| {
            Error::stop("the Python process of the packages sent a handle without a number")
        })?;
        Ok(Value::native(PackageObject {
            packages: packages.clone(),
            generation: packages.generation.get(),
            handle,
            type_name: fields
                .get("type")
                .and_then(Json::as_str)
                .unwrap_or("object")
                .to_owned(),
            callable: fields
                .get("callable")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        }))
    }

    /// The handle as the process takes it back, while its process runs.
    pub(super) fn reference(&self) -> Result<Json, Error> {
        if self.generation != self.packages.generation.get() || !self.packages.is_running() {
            return Err(Error::stop(format!(
                "workflow code uses a {} of a Python package after the Python process of the packages stopped",
                self.type_name
            )));
        }
        let mut object = Map::new();
        object.insert("handle".into(), Json::from(self.handle));
        Ok(Json::Object(object))
    }

    /// Sends a request about this object.
    fn ask(&self, op: &str, mut fields: Vec<(&'static str, Json)>) -> Result<Value, Error> {
        fields.insert(0, ("object", self.reference()?));
        self.packages.call(op, fields)
    }
}

impl Drop for PackageObject {
    fn drop(&mut self) {
        self.packages.release(self.generation, self.handle);
    }
}

impl Native for PackageObject {
    fn type_name(&self) -> &str {
        &self.type_name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        self.ask("getattr", vec![("name", Json::String(name.to_owned()))])
            .map(Some)
    }

    fn set_attribute(
        &self,
        interpreter: &mut Interpreter,
        name: &str,
        value: Value,
    ) -> Result<bool, Error> {
        let value = encode(interpreter, &value)?;
        self.ask(
            "setattr",
            vec![("name", Json::String(name.to_owned())), ("value", value)],
        )?;
        Ok(true)
    }

    fn call(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        args: Args,
    ) -> Result<Option<Value>, Error> {
        let mut positional = Vec::with_capacity(args.positional.len());
        for arg in &args.positional {
            positional.push(encode(interpreter, arg)?);
        }
        let mut keywords = Vec::with_capacity(args.keywords.len());
        for (name, value) in &args.keywords {
            keywords.push(Json::Array(vec![
                Json::String(name.to_string()),
                encode(interpreter, value)?,
            ]));
        }
        self.ask(
            "call",
            vec![
                ("args", Json::Array(positional)),
                ("kwargs", Json::Array(keywords)),
            ],
        )
        .map(Some)
    }

    fn get_item(
        &self,
        interpreter: &mut Interpreter,
        _: &Value,
        key: &Value,
    ) -> Result<Option<Value>, Error> {
        let key = encode(interpreter, key)?;
        self.ask("getitem", vec![("key", key)]).map(Some)
    }

    fn set_item(
        &self,
        interpreter: &mut Interpreter,
        key: &Value,
        value: Value,
    ) -> Result<bool, Error> {
        let key = encode(interpreter, key)?;
        let value = encode(interpreter, &value)?;
        self.ask("setitem", vec![("key", key), ("value", value)])?;
        Ok(true)
    }

    fn iterate(&self, interpreter: &mut Interpreter) -> Result<Option<Vec<Value>>, Error> {
        let items = self.ask("iter", Vec::new())?;
        interpreter.iterate(&items).map(Some)
    }

    fn contains(&self, interpreter: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        let item = encode(interpreter, item)?;
        let found = self.ask("contains", vec![("item", item)])?;
        Ok(Some(matches!(found, Value::Bool(true))))
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable" && self.callable
    }

}
