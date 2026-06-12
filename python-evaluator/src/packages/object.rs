//! `PackageObject`: the handle of an object that lives in the Python process of the packages.
//! Each use of it is one request to the process.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::rc::Rc;

use serde_json::{Map, Value as Json};

use super::Packages;
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{Native, Value};

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

    fn instance_of(&self, class: &str) -> bool {
        class == "callable" && self.callable
    }

}
