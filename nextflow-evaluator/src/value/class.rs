//! Class values, like `Path` in `addConverter(Path) { ... }` or the result of `getClass()`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The fully qualified names of the classes that scripts may name without a package.
const QUALIFIED: &[&str] = &[
    "groovy.lang.Closure",
    "java.io.File",
    "java.lang.Boolean",
    "java.lang.CharSequence",
    "java.lang.Double",
    "java.lang.Exception",
    "java.lang.Float",
    "java.lang.IllegalArgumentException",
    "java.lang.IllegalStateException",
    "java.lang.Integer",
    "java.lang.Iterable",
    "java.lang.Long",
    "java.lang.Math",
    "java.lang.Number",
    "java.lang.Object",
    "java.lang.RuntimeException",
    "java.lang.String",
    "java.lang.System",
    "java.math.BigDecimal",
    "java.math.BigInteger",
    "java.nio.file.Files",
    "java.nio.file.Path",
    "java.nio.file.Paths",
    "java.util.Arrays",
    "java.util.Collection",
    "java.util.Date",
    "java.util.List",
    "java.util.Locale",
    "java.util.Map",
    "java.util.Random",
    "java.util.Set",
    "java.util.UUID",
    "nextflow.Channel",
    "nextflow.script.types.VersionNumber",
    "nextflow.util.Duration",
    "nextflow.util.MemoryUnit",
];

/// Interfaces, which Java shows as `interface X` instead of `class X`.
const INTERFACES: &[&str] = &[
    "java.lang.CharSequence",
    "java.lang.Iterable",
    "java.nio.file.Path",
    "java.util.Collection",
    "java.util.List",
    "java.util.Map",
    "java.util.Set",
    "nextflow.script.types.VersionNumber",
];

/// The fully qualified name of a class that a script names by its simple name.
pub fn qualified_class_name(simple: &str) -> Option<&'static str> {
    QUALIFIED
        .iter()
        .find(|name| simple_class_name(name) == simple)
        .copied()
}

/// `Class.getSimpleName()`: the name without package or outer class.
pub fn simple_class_name(qualified: &str) -> &str {
    let name = qualified.rsplit('.').next().unwrap_or(qualified);
    name.rsplit('$').next().unwrap_or(name)
}

/// `Class.toString()`, like `interface java.nio.file.Path`.
pub fn class_text(qualified: &str) -> String {
    let kind = match INTERFACES.contains(&qualified) {
        true => "interface",
        false => "class",
    };
    format!("{kind} {qualified}")
}
