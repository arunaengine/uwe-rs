//! Verifier checks of the ops that select or cut the items of a stream: buffer, window,
//! take, unique, distinct, until, if_empty and split.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::callbacks::Arity;
use super::kinds::array_of;
use super::{Checker, Problem};
use crate::closure::Body;
use crate::document::Port;
use crate::flow::Flow;
use crate::op::{Effect, Op};
use crate::reserved::{
    Buffer, BufferRule, IfEmpty, Pair, SequenceField, Split, SplitFormat, Window,
};
use crate::types::Type;

impl<'d> Checker<'d> {
    pub(super) fn buffer(&mut self, at: &str, op: &Op, buffer: &Buffer) {
        let Some((source, out)) = self.streamed(at, op, "a buffer") else {
            return;
        };
        let item = [&source.r#type];
        match &buffer.rule {
            BufferRule::Size(rule) if rule.size == 0 => {
                self.report(
                    at,
                    Problem::Shape("a buffer".into(), "a positive size".into()),
                );
            }
            BufferRule::Size(_) => {}
            BufferRule::Match(rule) => {
                for body in rule.open.iter().chain([&rule.close]) {
                    self.body(at, op, body, (Arity::Unary, &item), Some(&Type::Bool));
                }
            }
        }
        self.gives(at, out, &array_of(source.r#type.clone()));
    }

    pub(super) fn window(&mut self, at: &str, op: &Op, window: &Window) {
        let Some((source, out)) = self.streamed(at, op, "a window") else {
            return;
        };
        if window.size == 0 || window.step == Some(0) {
            let why = "a positive size and step".into();
            self.report(at, Problem::Shape("a window".into(), why));
        }
        self.gives(at, out, &array_of(source.r#type.clone()));
    }

    /// An op that gives some of its items as they are, with an optional key body.
    pub(super) fn selects(&mut self, at: &str, op: &Op, what: &str, key: Option<&Body>) {
        let Some((source, out)) = self.streamed(at, op, what) else {
            return;
        };
        if let Some(key) = key {
            self.body(at, op, key, (Arity::Unary, &[&source.r#type]), None);
        }
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn until(&mut self, at: &str, op: &Op, predicate: &Body) {
        let Some((source, out)) = self.streamed(at, op, "an until") else {
            return;
        };
        let item = [&source.r#type];
        self.body(at, op, predicate, (Arity::Unary, &item), Some(&Type::Bool));
        self.gives(at, out, &source.r#type);
    }

    pub(super) fn if_empty(&mut self, at: &str, op: &Op, fallback: &IfEmpty) {
        let Some((source, out)) = self.ends(at, op, "an if_empty", (None, None)) else {
            return;
        };
        self.value_captures(at, op, "an if_empty", &["source"]);
        self.gives(at, out, &source.r#type);
        match (&fallback.value, &fallback.body) {
            (Some(literal), None) => {
                if !crate::fits(&literal.value, &out.r#type, self.defs) {
                    let named = format!("fallback value of output {}", out.name);
                    self.report(at, Problem::Unfit(named));
                }
            }
            (None, Some(body)) => self.body(at, op, body, (Arity::Zero, &[]), None),
            _ => {
                let why = "a value or a body, exactly one".into();
                self.report(at, Problem::Shape("an if_empty".into(), why));
            }
        }
    }

    /// The stream input `source` and the stream output `out` of an op.
    pub(super) fn streamed<'o>(
        &mut self,
        at: &str,
        op: &'o Op,
        what: &str,
    ) -> Option<(&'o Port, &'o Port)> {
        let stream = Some(Flow::Stream);
        let ends = self.ends(at, op, what, (stream, stream));
        self.value_captures(at, op, what, &["source"]);
        ends
    }
}

impl<'d> Checker<'d> {
    pub(super) fn split(&mut self, at: &str, op: &Op, split: &Split) {
        let Some((source, _)) = self.streamed(at, op, "a split") else {
            return;
        };
        let mut wrong = Vec::new();
        if split.size == 0 {
            wrong.push("a positive size".to_owned());
        }
        let (fields, fastq) = match &split.format {
            SplitFormat::Fasta(sequence) => (&sequence.fields[..], false),
            SplitFormat::Fastq(sequence) => (&sequence.fields[..], true),
            _ => (&[][..], false),
        };
        match (&split.pair, split.elem) {
            (Some(_), _) if !fastq => wrong.push("a pair only for fastq".into()),
            (Some(_), Some(_)) => wrong.push("a pair or an elem, not both".into()),
            (Some(Pair::At([first, second])), None) if first == second => {
                wrong.push("two different pair elements".into());
            }
            _ => {}
        }
        let mut names = BTreeSet::new();
        for field in fields {
            if field.field != SequenceField::Text && field.field.in_fasta() == fastq {
                wrong.push(format!("fields of its format, not {:?}", field.field));
            }
            if !names.insert(&field.name) {
                wrong.push(format!("one field named {}", field.name));
            }
        }
        let structured = matches!(split.format, SplitFormat::Csv(_) | SplitFormat::Json(_));
        if split.files.is_some() && (structured || !fields.is_empty()) {
            wrong.push("text chunks to write files".into());
        }
        if let SplitFormat::Json(json) = &split.format
            && !json_path(&json.path)
        {
            wrong.push(format!(
                "a JSON path of names and indices, not {:?}",
                json.path
            ));
        }
        if !CHARSETS.contains(&split.charset.to_ascii_lowercase().as_str()) {
            wrong.push(format!("a known charset, not {}", split.charset));
        }
        if let Some(files) = &split.files {
            wrong.extend(places(
                op,
                files.name.as_deref(),
                files.directory.as_deref(),
            ));
        }
        for why in wrong {
            self.report(at, Problem::Shape("a split".into(), why));
        }
        if let Some(body) = &split.body {
            self.body(at, op, body, (Arity::Unary, &[&source.r#type]), None);
        }
    }
}

/// A file name is a name, and a folder outside the run needs a publish effect.
pub(super) fn places(op: &Op, name: Option<&str>, directory: Option<&str>) -> Vec<String> {
    let mut wrong = Vec::new();
    if let Some(name) = name
        && (name.is_empty() || name.contains('/') || name == "..")
    {
        wrong.push(format!("a file name, not {name:?}"));
    }
    let Some(directory) = directory else {
        return wrong;
    };
    let outside = directory.starts_with('/') || directory.contains("://");
    let published = (op.effects.iter()).any(|effect| matches!(effect, Effect::Publish(_)));
    if outside && !published {
        wrong.push(format!("a publish effect to write to {directory}"));
    }
    if !outside && directory.split('/').any(|part| part == "..") {
        wrong.push(format!("a folder inside the run, not {directory}"));
    }
    wrong
}

/// The charsets a split reads.
const CHARSETS: [&str; 7] = [
    "utf-8",
    "utf8",
    "us-ascii",
    "ascii",
    "iso-8859-1",
    "latin-1",
    "latin1",
];

/// Whether a JSON path is dot-separated member names, each with optional bracket indices,
/// like `samples[0].files`; the empty path is the document.
fn json_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    path.split('.').enumerate().all(|(at, part)| {
        let (name, mut rest) = part.split_at(part.find('[').unwrap_or(part.len()));
        if name.contains(']') || (name.is_empty() && (at > 0 || rest.is_empty())) {
            return false;
        }
        while !rest.is_empty() {
            let Some((index, after)) = rest
                .strip_prefix('[')
                .and_then(|inner| inner.split_once(']'))
            else {
                return false;
            };
            if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            rest = after;
        }
        true
    })
}
