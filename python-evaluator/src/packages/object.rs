//! `PackageObject`: the handle of an object that lives in the Python process of the packages.
//! Each use of it is one request to the process.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;
use std::hash::Hasher;
use std::rc::Rc;

use serde_json::{Map, Value as Json};

use super::Packages;
use super::protocol::encode;
use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::value::{Args, Native, Value};

pub struct PackageObject {
    packages: Rc<Packages>,
    /// The process the handle belongs to.
    generation: u64,
    handle: u64,
    type_name: String,
    /// The name of the class and of its module, when the object is a class.
    class_name: Option<String>,
    class_module: Option<String>,
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
            class_name: fields.get("name").and_then(Json::as_str).map(str::to_owned),
            class_module: fields
                .get("module")
                .and_then(Json::as_str)
                .map(str::to_owned),
            callable: fields
                .get("callable")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        }))
    }

    /// The name of the class, when the object is a class.
    pub fn class_name(&self) -> Option<&str> {
        self.class_name.as_deref()
    }

    /// Whether the object is a class of exceptions of the package that an exception of the
    /// package is, or derives from, so that `except package.Error:` catches it.
    pub fn catches(&self, exception: &Value) -> bool {
        let (Some(module), Some(name)) = (&self.class_module, &self.class_name) else {
            return false;
        };
        let wanted = format!("{module}.{name}");
        let classes = crate::types::class_of(exception).lookup(super::protocol::CLASSES);
        match classes {
            Some(Value::Tuple(classes)) => classes
                .iter()
                .any(|class| class.as_str() == Some(wanted.as_str())),
            _ => false,
        }
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

/// The name of an operator in Python's `operator` module.
fn operator_name(operator: Operator) -> &'static str {
    match operator {
        Operator::Add => "add",
        Operator::Sub => "sub",
        Operator::Mul => "mul",
        Operator::MatMul => "matmul",
        Operator::TrueDiv => "truediv",
        Operator::FloorDiv => "floordiv",
        Operator::Mod => "mod",
        Operator::Pow => "pow",
        Operator::LShift => "lshift",
        Operator::RShift => "rshift",
        Operator::BitAnd => "and_",
        Operator::BitOr => "or_",
        Operator::BitXor => "xor",
        Operator::Eq => "eq",
        Operator::NotEq => "ne",
        Operator::Lt => "lt",
        Operator::LtE => "le",
        Operator::Gt => "gt",
        Operator::GtE => "ge",
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

    fn length(&self) -> Option<usize> {
        self.ask("len", Vec::new())
            .ok()
            .and_then(|length| length.as_int())
            .and_then(|length| usize::try_from(length).ok())
    }

    fn contains(&self, interpreter: &mut Interpreter, item: &Value) -> Result<Option<bool>, Error> {
        let item = encode(interpreter, item)?;
        let found = self.ask("contains", vec![("item", item)])?;
        Ok(Some(matches!(found, Value::Bool(true))))
    }

    fn truthy(&self) -> Option<bool> {
        match self.ask("bool", Vec::new()) {
            Ok(Value::Bool(flag)) => Some(flag),
            _ => None,
        }
    }

    fn to_str(&self, _: &mut Interpreter) -> Result<Option<String>, Error> {
        let text = self.ask("str", Vec::new())?;
        Ok(text.as_str().map(str::to_owned))
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        let text = self.ask("repr", Vec::new())?;
        Ok(text.as_str().unwrap_or_default().to_owned())
    }

    fn binary(
        &self,
        interpreter: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let this = self.reference()?;
        let other = encode(interpreter, other)?;
        let (left, right) = if reflected {
            (other, this)
        } else {
            (this, other)
        };
        self.packages
            .call(
                "binary",
                vec![
                    ("operator", Json::String(operator_name(operator).to_owned())),
                    ("left", left),
                    ("right", right),
                ],
            )
            .map(Some)
    }

    fn unary(&self, _: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        let name = match operator {
            "-" => "neg",
            "+" => "pos",
            "~" => "invert",
            _ => return Ok(None),
        };
        self.ask("unary", vec![("operator", Json::String(name.to_owned()))])
            .map(Some)
    }

    fn key_equal(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<PackageObject>()
            .is_some_and(|other| other.handle == self.handle && other.generation == self.generation)
    }

    fn key_hash(&self, state: &mut dyn Hasher) {
        state.write_u64(self.handle);
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "callable" && self.callable
    }

    fn check_instance(&self, value: &Value) -> Option<bool> {
        let object = match value.as_native::<PackageObject>() {
            Some(object) => object.reference().ok()?,
            None => return Some(false),
        };
        let class = self.reference().ok()?;
        match self
            .packages
            .call("isinstance", vec![("object", object), ("class", class)])
        {
            Ok(Value::Bool(flag)) => Some(flag),
            _ => None,
        }
    }
}
