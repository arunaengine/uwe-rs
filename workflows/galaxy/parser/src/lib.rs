//! Reads Galaxy tool XML, expands its macros, and gives a typed tool and a tool definition.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod macros;
pub mod xml;

/// Why a tool could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{file}:{line}: the XML is not valid: {source}")]
    Xml {
        file: String,
        line: usize,
        #[source]
        source: quick_xml::Error,
    },
    #[error("{file}:{line}: {message}")]
    Document {
        file: String,
        line: usize,
        message: String,
    },
    #[error("cannot read the macro file {path}: {source}")]
    Import {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("macro {macro_name}: {message}")]
    Macro { macro_name: String, message: String },
    #[error("tool {tool}: {message}")]
    Tool { tool: String, message: String },
}
