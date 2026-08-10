//! Chooses one package for each name that an environment needs: a search that takes the
//! newest package of the first channel that has the name, adds what it depends on, and goes
//! back to the next choice when a later package cannot fit. It is no full solver like conda's:
//! it gives up after a number of tries and says so, rather than search for long.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use crate::repodata::{Index, Record};
use crate::spec::MatchSpec;
use crate::{LockError, SpecError};

/// How many packages the search may try before it gives up.
const TRIES: usize = 200_000;

/// Virtual packages that stand for what the machine has, like `__glibc`: a container of a
/// Linux image has them. Others, like `__cuda`, it lacks.
const PRESENT: &[&str] = &["__unix", "__linux", "__glibc", "__archspec"];

/// A point where the search chose among packages, to go back to.
struct Choice<'a> {
    /// The place of the spec in the queue.
    spec: usize,
    candidates: Vec<&'a Record>,
    /// The candidate chosen now.
    at: usize,
    /// The length of the queue before the chosen package added what it depends on.
    queue: usize,
}

/// What the search goes through: the specs to fit, in order, and the packages chosen so far.
struct Search<'a> {
    index: &'a Index,
    queue: Vec<MatchSpec>,
    chosen: HashMap<String, &'a Record>,
    /// Each point of the queue: a choice, or a spec that a chosen package fit already.
    trail: Vec<Option<Choice<'a>>>,
    tries: usize,
}

/// The packages that fit every spec and everything that they depend on, sorted by name.
pub(crate) fn solve<'a>(
    index: &'a Index,
    specs: &[MatchSpec],
) -> Result<Vec<&'a Record>, LockError> {
    let mut search = Search {
        index,
        queue: specs.to_vec(),
        chosen: HashMap::new(),
        trail: Vec::new(),
        tries: 0,
    };
    // The first spec that no package can fit at all names the cause better than a search.
    for spec in specs {
        if !present(&spec.name) && candidates(index, spec).is_empty() {
            return Err(LockError::NoPackage {
                spec: spec.to_string(),
                channels: index.channels.clone(),
            });
        }
    }
    search.run(specs)?;
    let mut records: Vec<&Record> = search.chosen.into_values().collect();
    records.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(records)
}

/// Whether a virtual package is on the machine.
fn present(name: &str) -> bool {
    name.starts_with("__") && PRESENT.contains(&name)
}

/// The packages that could fit a spec, newest first.
fn candidates<'a>(index: &'a Index, spec: &MatchSpec) -> Vec<&'a Record> {
    (index
        .candidates(&spec.name, spec.channel.as_deref())
        .into_iter())
    .filter(|record| spec.matches(record))
    .collect()
}

impl<'a> Search<'a> {
    fn run(&mut self, specs: &[MatchSpec]) -> Result<(), LockError> {
        let mut next = 0;
        while next < self.queue.len() {
            let spec = self.queue[next].clone();
            let step = match self.chosen.get(&spec.name) {
                _ if spec.name.starts_with("__") => present(&spec.name).then_some(None),
                Some(record) => spec.matches(record).then_some(None),
                None => self.choose(next, &spec)?,
            };
            match step {
                Some(point) => {
                    self.trail.push(point);
                    next += 1;
                }
                None => next = self.back(specs)?,
            }
        }
        Ok(())
    }

    /// Chooses the first package that fits a spec and the packages chosen so far; `None`
    /// when none fits.
    fn choose(
        &mut self,
        at: usize,
        spec: &MatchSpec,
    ) -> Result<Option<Option<Choice<'a>>>, LockError> {
        let mut choice = Choice {
            spec: at,
            candidates: candidates(self.index, spec),
            at: 0,
            queue: self.queue.len(),
        };
        match self.take(&mut choice)? {
            true => Ok(Some(Some(choice))),
            false => Ok(None),
        }
    }

    /// Takes the next candidate of a choice that fits, from `choice.at` on.
    fn take(&mut self, choice: &mut Choice<'a>) -> Result<bool, LockError> {
        while choice.at < choice.candidates.len() {
            self.tries += 1;
            if self.tries > TRIES {
                return Err(LockError::TooComplex { tries: TRIES });
            }
            let record = choice.candidates[choice.at];
            if self.fits(record) {
                self.chosen.insert(record.name.clone(), record);
                for depends in &record.depends {
                    let depends =
                        MatchSpec::parse(depends).map_err(|error| LockError::Package {
                            package: format!("{}-{}-{}", record.name, record.version, record.build),
                            error,
                        })?;
                    self.queue.push(depends);
                }
                return Ok(true);
            }
            choice.at += 1;
        }
        Ok(false)
    }

    /// Whether a package fits the packages chosen so far: what they constrain, and what it
    /// constrains.
    fn fits(&self, record: &Record) -> bool {
        let constraint = |text: &String| MatchSpec::parse(text).ok();
        let theirs = (self.chosen.values())
            .flat_map(|chosen| chosen.constrains.iter().filter_map(constraint))
            .filter(|spec| spec.name == record.name)
            .all(|spec| spec.matches(record));
        let mine = (record.constrains.iter().filter_map(constraint)).all(|spec| {
            self.chosen
                .get(&spec.name)
                .is_none_or(|chosen| spec.matches(chosen))
        });
        theirs && mine
    }

    /// Goes back to the last choice that has another candidate, takes it, and returns where
    /// the search goes on.
    fn back(&mut self, specs: &[MatchSpec]) -> Result<usize, LockError> {
        while let Some(point) = self.trail.pop() {
            let Some(mut choice) = point else {
                continue;
            };
            let name = &self.queue[choice.spec].name;
            self.chosen.remove(name);
            self.queue.truncate(choice.queue);
            choice.at += 1;
            if self.take(&mut choice)? {
                let next = choice.spec + 1;
                self.trail.push(Some(choice));
                return Ok(next);
            }
        }
        Err(LockError::Conflict {
            specs: specs.iter().map(ToString::to_string).collect(),
        })
    }
}

/// Keeps [`SpecError`] in the errors of this module.
impl From<SpecError> for LockError {
    fn from(error: SpecError) -> Self {
        LockError::Spec(error)
    }
}
