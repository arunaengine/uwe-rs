//! Verifier checks of dialect tasks: one task form, the pinned source of the builder, and
//! the native selectors of its port maps.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use super::{Checker, Problem, show};
use crate::dialect::{PortMap, TaskBuilder};
use crate::document::{Language, Port};
use crate::op::{Op, Task};

impl<'d> Checker<'d> {
    pub(super) fn builder(&mut self, at: &str, op: &Op, task: &Task, builder: &TaskBuilder) {
        let command = !task.command.is_empty() || !task.captures.is_empty();
        let fields = command || task.image.is_some() || !task.success_codes.is_empty();
        let generic = task.accept.is_some() || task.retry.is_some();
        if fields || generic || task.stdout.is_some() || task.stderr.is_some() || task.network {
            let why = "no command, capture, acceptance or retry fields beside a builder".into();
            self.report(at, Problem::Shape("a task".into(), why));
        }
        let language = builder.language();
        let reference = builder.task();
        for id in [&reference.source].into_iter().chain(&reference.configs) {
            let Some(source) = self.source(at, id) else {
                continue;
            };
            if source.language != language {
                let (found, want) = (show(&language), show(&source.language));
                self.report(
                    at,
                    Problem::Language {
                        named: id.clone(),
                        found,
                        want,
                    },
                );
            }
            if source.digest.is_none() {
                let why = format!("a digest that pins source {id}");
                self.report(at, Problem::Shape("a dialect task".into(), why));
            }
        }
        if reference.entry.is_empty() || reference.call.is_empty() {
            let why = "an entry and a call".into();
            self.report(at, Problem::Shape("a dialect task".into(), why));
        }
        self.captures(at, op, &reference.captures);
        if let Some(scope) = &reference.scope
            && let Some(source) = self.source(at, &scope.source)
            && source.language != language
        {
            let named = scope.source.clone();
            let (found, want) = (show(&language), show(&source.language));
            self.report(at, Problem::Language { named, found, want });
        }
        self.port_maps(at, "input", &op.inputs, &reference.inputs, language);
        self.port_maps(at, "output", &op.outputs, &reference.outputs, language);
    }

    /// Each port is mapped once, to a selector of the language.
    fn port_maps(
        &mut self,
        at: &str,
        side: &str,
        ports: &[Port],
        maps: &[PortMap],
        language: Language,
    ) {
        let mut seen = BTreeSet::new();
        for map in maps {
            if !ports.iter().any(|port| port.name == map.port) {
                self.report(at, Problem::NoPort(format!("{side} {}", map.port)));
            }
            if !seen.insert(&map.port) {
                self.report(
                    at,
                    Problem::Duplicate(format!("the map of {side} {}", map.port)),
                );
            }
            if !selector(language, side, &map.native) {
                let why = format!("a {} selector, not {:?}", show(&language), map.native);
                self.report(at, Problem::Shape("a port map".into(), why));
            }
        }
        for port in ports
            .iter()
            .filter(|port| !maps.iter().any(|map| map.port == port.name))
        {
            let why = format!("a map of {side} {}", port.name);
            self.report(at, Problem::Shape("a dialect task".into(), why));
        }
    }
}

/// Whether `native` is a selector of `language` on that side: `<side>/<index>` for Nextflow,
/// also `params/<name>` for Snakemake inputs, `parameter/<name>` or `artifact/<name>` for
/// Argo, and a declared name for WDL.
fn selector(language: Language, side: &str, native: &str) -> bool {
    let index = |rest: &str| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit());
    let name = |rest: &str| !rest.is_empty() && !rest.contains('/');
    match (language, native.split_once('/')) {
        (Language::Nextflow, Some((kind, rest))) => kind == side && index(rest),
        (Language::Snakemake, Some(("params", rest))) => side == "input" && name(rest),
        (Language::Snakemake, Some((kind, rest))) => kind == side && index(rest),
        (Language::Argo, Some(("parameter" | "artifact", rest))) => name(rest),
        (Language::Wdl, None) => name(native),
        _ => false,
    }
}
