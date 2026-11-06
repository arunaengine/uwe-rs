//! Java exceptions and errors, as scripts see and catch them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use crate::number::ArithmeticError;

/// A thrown Java exception or error: its simple class name, its message and the exception
/// that caused it, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    pub class: &'static str,
    pub message: String,
    pub cause: Option<Box<Exception>>,
    /// What some classes carry besides the message, as JSON text, like the violation of an
    /// everit `ValidationException`, which `toJSON()` gives.
    pub details: Option<String>,
}

impl Exception {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            cause: None,
            details: None,
        }
    }

    pub fn illegal_argument(message: impl Into<String>) -> Self {
        Self::new("IllegalArgumentException", message)
    }

    /// Java's `NumberFormatException` for text that is not a number.
    pub fn number_format(text: &str) -> Self {
        Self::new(
            "NumberFormatException",
            format!("For input string: \"{text}\""),
        )
    }

    /// A failed Groovy `assert`. It is an `Error`, which `catch (e)` does not catch.
    pub fn assertion(message: impl Into<String>) -> Self {
        Self::new("AssertionError", message)
    }
}

/// Exception classes by simple name, with the qualified name and the parent class.
const CLASSES: &[(&str, &str, &str)] = &[
    ("Throwable", "java.lang.Throwable", ""),
    ("Exception", "java.lang.Exception", "Throwable"),
    ("Error", "java.lang.Error", "Throwable"),
    (
        "ReflectiveOperationException",
        "java.lang.ReflectiveOperationException",
        "Exception",
    ),
    (
        "InvocationTargetException",
        "java.lang.reflect.InvocationTargetException",
        "ReflectiveOperationException",
    ),
    ("AssertionError", "java.lang.AssertionError", "Error"),
    (
        "VirtualMachineError",
        "java.lang.VirtualMachineError",
        "Error",
    ),
    (
        "StackOverflowError",
        "java.lang.StackOverflowError",
        "VirtualMachineError",
    ),
    (
        "RuntimeException",
        "java.lang.RuntimeException",
        "Exception",
    ),
    (
        "ArithmeticException",
        "java.lang.ArithmeticException",
        "RuntimeException",
    ),
    (
        "ClassCastException",
        "java.lang.ClassCastException",
        "RuntimeException",
    ),
    (
        "IllegalArgumentException",
        "java.lang.IllegalArgumentException",
        "RuntimeException",
    ),
    (
        "NumberFormatException",
        "java.lang.NumberFormatException",
        "IllegalArgumentException",
    ),
    (
        "IllegalStateException",
        "java.lang.IllegalStateException",
        "RuntimeException",
    ),
    (
        "IndexOutOfBoundsException",
        "java.lang.IndexOutOfBoundsException",
        "RuntimeException",
    ),
    (
        "ArrayIndexOutOfBoundsException",
        "java.lang.ArrayIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
    ),
    (
        "StringIndexOutOfBoundsException",
        "java.lang.StringIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
    ),
    (
        "NullPointerException",
        "java.lang.NullPointerException",
        "RuntimeException",
    ),
    (
        "NoSuchElementException",
        "java.util.NoSuchElementException",
        "RuntimeException",
    ),
    ("IOException", "java.io.IOException", "Exception"),
    (
        "ProcessException",
        "nextflow.exception.ProcessException",
        "Exception",
    ),
    (
        "DuplicateProcessInvocation",
        "nextflow.exception.DuplicateProcessInvocation",
        "ProcessException",
    ),
    (
        "NoSuchFileException",
        "java.nio.file.NoSuchFileException",
        "IOException",
    ),
    ("EOFException", "java.io.EOFException", "IOException"),
    (
        "FileNotFoundException",
        "java.io.FileNotFoundException",
        "IOException",
    ),
    ("ZipException", "java.util.zip.ZipException", "IOException"),
    ("ParseException", "java.text.ParseException", "Exception"),
    (
        "JsonException",
        "groovy.json.JsonException",
        "RuntimeException",
    ),
    (
        "YAMLException",
        "org.yaml.snakeyaml.error.YAMLException",
        "RuntimeException",
    ),
    (
        "SchemaValidationException",
        "nextflow.validation.exceptions.SchemaValidationException",
        "Exception",
    ),
    (
        "MultipleCompilationErrorsException",
        "org.codehaus.groovy.control.MultipleCompilationErrorsException",
        "GroovyRuntimeException",
    ),
    (
        "UnsupportedOperationException",
        "java.lang.UnsupportedOperationException",
        "RuntimeException",
    ),
    (
        "GroovyRuntimeException",
        "groovy.lang.GroovyRuntimeException",
        "RuntimeException",
    ),
    (
        "MissingMethodException",
        "groovy.lang.MissingMethodException",
        "GroovyRuntimeException",
    ),
    (
        "MissingPropertyException",
        "groovy.lang.MissingPropertyException",
        "GroovyRuntimeException",
    ),
    (
        "AbortOperationException",
        "nextflow.exception.AbortOperationException",
        "RuntimeException",
    ),
    (
        "JSONException",
        "org.json.JSONException",
        "RuntimeException",
    ),
    (
        "ValidationException",
        "org.everit.json.schema.ValidationException",
        "RuntimeException",
    ),
];

/// The static simple name of a known exception class, as `new` and `catch` name it.
pub fn class_name(name: &str) -> Option<&'static str> {
    CLASSES
        .iter()
        .find(|(class, _, _)| *class == name)
        .map(|(class, _, _)| *class)
}

impl Exception {
    /// The fully qualified class name, or the simple name for a class outside the table.
    pub fn qualified_class(&self) -> &'static str {
        CLASSES
            .iter()
            .find(|(name, _, _)| *name == self.class)
            .map_or(self.class, |(_, qualified, _)| qualified)
    }

    /// The exception that `InvocationTargetException`s wrap, which Nextflow reports instead.
    pub fn target(&self) -> &Exception {
        match &self.cause {
            Some(cause) if self.class == "InvocationTargetException" => cause.target(),
            _ => self,
        }
    }

    /// Whether the exception is an instance of the class, as a `catch` clause tests it.
    pub fn is_instance(&self, class: &str) -> bool {
        let mut current = self.class;
        loop {
            if current == class {
                return true;
            }
            match CLASSES.iter().find(|(name, _, _)| *name == current) {
                Some((_, _, parent)) if !parent.is_empty() => current = parent,
                _ => return class == "Throwable",
            }
        }
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}

impl std::error::Error for Exception {}

impl From<ArithmeticError> for Exception {
    fn from(error: ArithmeticError) -> Self {
        match error {
            ArithmeticError::Arithmetic(message) => Self::new("ArithmeticException", message),
            ArithmeticError::Unsupported(message) => {
                Self::new("UnsupportedOperationException", message)
            }
        }
    }
}
