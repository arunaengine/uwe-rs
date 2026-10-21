//! Complex numbers, as `complex(1, 2)` and `2j` make them: arithmetic, comparison for
//! equality, `abs`, and the text Python writes for them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::any::Any;

use crate::error::Error;
use crate::interpreter::{Interpreter, Operator};
use crate::number::{float_repr, to_f64};
use crate::value::{Native, Value};

/// A complex number by its real and imaginary parts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Complex(pub f64, pub f64);

impl Complex {
    pub fn value(real: f64, imag: f64) -> Value {
        Value::native(Complex(real, imag))
    }

    /// A number as a complex number, if it is one.
    fn of(value: &Value) -> Option<Complex> {
        match value.as_native::<Complex>() {
            Some(complex) => Some(*complex),
            None => to_f64(value).map(|real| Complex(real, 0.0)),
        }
    }

    /// `complex(real=0, imag=0)`.
    pub fn construct(real: Option<&Value>, imag: Option<&Value>) -> Result<Value, Error> {
        let part = |value: Option<&Value>| -> Result<Complex, Error> {
            match value {
                None => Ok(Complex(0.0, 0.0)),
                Some(value) => Complex::of(value).ok_or_else(|| {
                    Error::type_error(
                        "complex() first argument must be a string or a number".to_owned(),
                    )
                }),
            }
        };
        let (real, imag) = (part(real)?, part(imag)?);
        // complex(a, b) is a + b * 1j.
        Ok(Complex::value(real.0 - imag.1, real.1 + imag.0))
    }
}

/// A part as Python writes it in a complex number: without `.0` for whole numbers.
fn part_text(value: f64) -> String {
    let text = float_repr(value);
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

impl Native for Complex {
    fn type_name(&self) -> &str {
        "complex"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn instance_of(&self, class: &str) -> bool {
        class == "complex"
    }

    fn get_attribute(
        &self,
        _: &mut Interpreter,
        _: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let this = *self;
        Ok(Some(match name {
            "real" => Value::Float(self.0),
            "imag" => Value::Float(self.1),
            "conjugate" => {
                Value::builtin("conjugate", move |_, _| Ok(Complex::value(this.0, -this.1)))
            }
            _ => return Ok(None),
        }))
    }

    fn binary(
        &self,
        _: &mut Interpreter,
        operator: Operator,
        other: &Value,
        reflected: bool,
    ) -> Result<Option<Value>, Error> {
        let Some(other) = Complex::of(other) else {
            return Ok(None);
        };
        let (a, b) = if reflected {
            (other, *self)
        } else {
            (*self, other)
        };
        Ok(Some(match operator {
            Operator::Add => Complex::value(a.0 + b.0, a.1 + b.1),
            Operator::Sub => Complex::value(a.0 - b.0, a.1 - b.1),
            Operator::Mul => Complex::value(a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0),
            Operator::TrueDiv => {
                let divisor = b.0 * b.0 + b.1 * b.1;
                if divisor == 0.0 {
                    return Err(Error::new("ZeroDivisionError", "division by zero"));
                }
                Complex::value(
                    (a.0 * b.0 + a.1 * b.1) / divisor,
                    (a.1 * b.0 - a.0 * b.1) / divisor,
                )
            }
            Operator::Eq => Value::Bool(a == b),
            Operator::NotEq => Value::Bool(a != b),
            _ => return Ok(None),
        }))
    }

    fn unary(&self, _: &mut Interpreter, operator: &str) -> Result<Option<Value>, Error> {
        Ok(match operator {
            "-" => Some(Complex::value(-self.0, -self.1)),
            "+" => Some(Complex::value(self.0, self.1)),
            _ => None,
        })
    }

    fn truthy(&self) -> Option<bool> {
        Some(self.0 != 0.0 || self.1 != 0.0)
    }

    fn repr(&self, _: &mut Interpreter) -> Result<String, Error> {
        let imag = part_text(self.1);
        if self.0 == 0.0 && self.0.is_sign_positive() {
            return Ok(format!("{imag}j"));
        }
        let sign = if self.1.is_sign_negative() || self.1.is_nan() && imag.starts_with('-') {
            ""
        } else {
            "+"
        };
        Ok(format!("({}{sign}{imag}j)", part_text(self.0)))
    }

    fn to_str(&self, interpreter: &mut Interpreter) -> Result<Option<String>, Error> {
        self.repr(interpreter).map(Some)
    }
}
