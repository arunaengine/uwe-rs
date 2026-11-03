//! Conda dependency resolution through rattler's resolvo backend.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use rattler_conda_types::package::BuildString;
use rattler_conda_types::{
    Flag, GenericVirtualPackage, Matches, ParseMatchSpecOptions, RepoDataRecord, RepodataRevision,
    StringMatcher,
};
use rattler_solve::{ChannelPriority, SolveError, SolverImpl, SolverTask};
use sha2::{Digest as _, Sha256};

use crate::{Index, LockError, MatchSpec, Record, SpecError};

pub(crate) fn solve(
    index: &Index,
    specs: &[MatchSpec],
    virtual_records: Option<&[Record]>,
) -> Result<Vec<Record>, LockError> {
    let virtual_packages = virtual_records
        .unwrap_or_default()
        .iter()
        .map(|record| {
            Ok(GenericVirtualPackage {
                name: record.name.parse().map_err(|_| SpecError {
                    text: record.name.clone(),
                    reason: "invalid virtual package name",
                })?,
                version: record
                    .version()
                    .ok_or_else(|| SpecError {
                        text: record.version.clone(),
                        reason: "invalid virtual package version",
                    })?
                    .0,
                build_string: BuildString::new_unchecked(record.build.clone()),
            })
        })
        .collect::<Result<Vec<_>, LockError>>()?;
    resolve(index.available(), specs, virtual_packages, &index.channels)?
        .into_iter()
        .map(|native| {
            let channel = index
                .channels
                .iter()
                .find(|channel| {
                    native
                        .channel
                        .as_deref()
                        .is_some_and(|url| url.trim_end_matches('/') == crate::base(channel))
                })
                .cloned()
                .unwrap_or_default();
            Record::from_native(native, channel)
                .map_err(|error| LockError::Solver(error.to_string()))
        })
        .collect()
}

pub(crate) fn resolve(
    available: Vec<Vec<&RepoDataRecord>>,
    specs: &[MatchSpec],
    virtual_packages: Vec<GenericVirtualPackage>,
    channels: &[String],
) -> Result<Vec<RepoDataRecord>, LockError> {
    for spec in specs {
        let found = if spec.name.starts_with("__") {
            virtual_packages
                .iter()
                .any(|package| spec.native.matches(package))
        } else {
            available.iter().flatten().any(|record| {
                spec.native.matches(*record)
                    && spec.native.channel.as_ref().is_none_or(|channel| {
                        record.channel.as_deref().is_some_and(|url| {
                            url.trim_end_matches('/')
                                == channel.base_url.as_str().trim_end_matches('/')
                        })
                    })
                    && spec
                        .native
                        .subdir
                        .as_ref()
                        .is_none_or(|subdir| subdir == &record.package_record.subdir)
                    && spec
                        .native
                        .file_name
                        .as_ref()
                        .is_none_or(|file| file == &record.identifier.to_file_name())
            })
        };
        if !found {
            return Err(LockError::NoPackage {
                spec: spec.to_string(),
                channels: channels.to_vec(),
            });
        }
    }
    // Rattler matches these fields only for the first root channel spec. Encode them
    // as variant flags so dependencies and optional constraints keep their qualifiers.
    let prepared = available
        .iter()
        .map(|bucket| {
            bucket
                .iter()
                .map(|record| {
                    let mut prepared = (*record).clone();
                    for (kind, value) in [
                        (
                            "channel",
                            record
                                .channel
                                .as_deref()
                                .unwrap_or_default()
                                .trim_end_matches('/')
                                .to_owned(),
                        ),
                        ("subdir", record.package_record.subdir.clone()),
                        ("file", record.identifier.to_file_name()),
                    ] {
                        prepared
                            .package_record
                            .flags
                            .push(Flag::new_unchecked(qualifier(kind, &value)));
                    }
                    for dependency in prepared
                        .package_record
                        .depends
                        .iter_mut()
                        .chain(&mut prepared.package_record.constrains)
                    {
                        if dependency.contains("::") || dependency.contains('[') {
                            let parsed = rattler_conda_types::MatchSpec::from_str(
                                dependency,
                                ParseMatchSpecOptions::lenient()
                                    .with_repodata_revision(RepodataRevision::V3),
                            )
                            .map_err(|error| LockError::Solver(error.to_string()))?;
                            *dependency = qualified(parsed).to_string();
                        }
                    }
                    Ok(prepared)
                })
                .collect::<Result<Vec<_>, LockError>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut task: SolverTask<_> = prepared.iter().map(|bucket| bucket.iter()).collect();
    task.specs = specs
        .iter()
        .map(|spec| qualified(spec.native.clone()))
        .collect();
    task.virtual_packages = virtual_packages;
    task.channel_priority = ChannelPriority::Flexible;
    let mut records = rattler_solve::resolvo::Solver
        .solve(task)
        .map_err(|error| match error {
            SolveError::Unsolvable(_) => LockError::Conflict {
                specs: specs.iter().map(ToString::to_string).collect(),
            },
            other => LockError::Solver(other.to_string()),
        })?
        .records;
    records = records
        .into_iter()
        .map(|selected| {
            available
                .iter()
                .flatten()
                .find(|original| original.url == selected.url)
                .map(|original| (**original).clone())
                .expect("solver returned an available record")
        })
        .collect();
    records.sort_by(|a, b| a.package_record.name.cmp(&b.package_record.name));
    Ok(records)
}

fn qualifier(kind: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("uwe-{kind}-{hex}")
}

fn qualified(mut spec: rattler_conda_types::MatchSpec) -> rattler_conda_types::MatchSpec {
    for (kind, value) in [
        (
            "channel",
            spec.channel
                .take()
                .map(|channel| channel.base_url.as_str().trim_end_matches('/').to_owned()),
        ),
        ("subdir", spec.subdir.take()),
        ("file", spec.file_name.take()),
    ] {
        if let Some(value) = value {
            spec.flags
                .get_or_insert_default()
                .push(StringMatcher::Exact(qualifier(kind, &value)));
        }
    }
    spec
}

impl From<SpecError> for LockError {
    fn from(error: SpecError) -> Self {
        LockError::Spec(error)
    }
}
