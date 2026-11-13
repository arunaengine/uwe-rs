//! Class values, like `Path` in `addConverter(Path) { ... }` or the result of `getClass()`.
// Copyright (c) 2026 The uwe Contributors
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
    "java.security.MessageDigest",
    "java.util.ArrayList",
    "java.util.Arrays",
    "java.util.Collection",
    "java.util.Date",
    "java.util.LinkedHashMap",
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

/// `to.isAssignableFrom(from)`: whether a value of the class `from` is also a `to`.
pub fn assignable(to: &str, from: &str) -> bool {
    if to == from || to == "java.lang.Object" {
        return true;
    }
    // An array class like `[Ljava.lang.String;` is assignable to the arrays of a supertype.
    let element = |name: &str| {
        name.strip_prefix("[L")?
            .strip_suffix(';')
            .map(str::to_owned)
    };
    if let (Some(to), Some(from)) = (element(to), element(from)) {
        return assignable(&to, &from);
    }
    let supertypes: &[&str] = match from {
        "java.util.ArrayList"
        | "java.util.LinkedList"
        | "nextflow.util.ArrayTuple"
        | "nextflow.util.BlankSeparatedList"
        | "groovy.lang.IntRange"
        | "groovy.lang.NumberRange"
        | "groovy.lang.ObjectRange"
        | "groovy.lang.EmptyRange" => &[
            "java.util.List",
            "java.util.Collection",
            "java.lang.Iterable",
        ],
        "java.util.LinkedHashSet" | "java.util.HashSet" => &[
            "java.util.Set",
            "java.util.Collection",
            "java.lang.Iterable",
        ],
        "java.util.LinkedHashMap" | "nextflow.util.RecordMap" | "java.util.TreeMap" => {
            &["java.util.Map"]
        }
        "nextflow.config.ConfigMap" => &["java.util.LinkedHashMap", "java.util.Map"],
        "java.lang.String" | "org.codehaus.groovy.runtime.GStringImpl" => {
            &["java.lang.CharSequence"]
        }
        "java.lang.Integer"
        | "java.lang.Long"
        | "java.lang.Double"
        | "java.lang.Float"
        | "java.math.BigDecimal"
        | "java.math.BigInteger" => &["java.lang.Number"],
        _ => &[],
    };
    supertypes.contains(&to)
}
