//! Built-in names that a script can use without declaring them, from nf-lang's DSL classes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Constants of every script (`ScriptDsl`).
pub(super) const SCRIPT_CONSTANTS: &[&str] = &[
    "baseDir",
    "channel",
    "launchDir",
    "log",
    "moduleDir",
    "nextflow",
    "projectDir",
    "secrets",
    "workDir",
    "workflow",
];

/// Constants of the entry workflow. Other code may read them, and Nextflow only warns; there,
/// assigning them declares a variable, as nf-core modules do with `args = task.ext.args`.
pub(super) const ENTRY_CONSTANTS: &[&str] = &["args", "params"];

/// Packages whose classes scripts may name in full, like `java.util.UUID`.
pub(super) const PACKAGES: &[&str] = &["groovy", "java", "javax", "nextflow", "org"];

/// Functions of every script (`ScriptDsl`).
pub(super) const SCRIPT_FUNCTIONS: &[&str] = &[
    "branchCriteria",
    "env",
    "error",
    "exit",
    "file",
    "files",
    "groupKey",
    "multiMapCriteria",
    "print",
    "printf",
    "println",
    "record",
    "sendMail",
    "sleep",
    "tuple",
];

/// Process directives (`ProcessDsl.DirectiveDsl`).
pub(super) const DIRECTIVES: &[&str] = &[
    "accelerator",
    "afterScript",
    "arch",
    "array",
    "beforeScript",
    "cache",
    "clusterOptions",
    "conda",
    "container",
    "containerOptions",
    "cpus",
    "debug",
    "disk",
    "errorStrategy",
    "executor",
    "ext",
    "fair",
    "hints",
    "label",
    "machineType",
    "maxErrors",
    "maxForks",
    "maxRetries",
    "maxSubmitAwait",
    "memory",
    "module",
    "penv",
    "pod",
    "publishDir",
    "queue",
    "resourceLabels",
    "resourceLimits",
    "scratch",
    "secret",
    "shell",
    "spack",
    "stageInMode",
    "stageOutMode",
    "storeDir",
    "tag",
    "time",
];

/// Classes that scripts may name, from the default imports of Groovy and Nextflow.
pub(super) const CLASSES: &[&str] = &[
    "BigDecimal",
    "BigInteger",
    "Boolean",
    "CharSequence",
    "Channel",
    "Closure",
    "Collection",
    "Date",
    "Double",
    "Duration",
    "Exception",
    "File",
    "Files",
    "Float",
    "IllegalArgumentException",
    "IllegalStateException",
    "Integer",
    "Iterable",
    "List",
    "Long",
    "Map",
    "Math",
    "MemoryUnit",
    "Number",
    "Object",
    "Path",
    "Paths",
    "Random",
    "Record",
    "RuntimeException",
    "Set",
    "String",
    "System",
    "Tuple",
    "UUID",
    "Value",
    "VersionNumber",
];
