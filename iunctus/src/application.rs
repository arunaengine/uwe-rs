//! Structured workflow description, admission and execution for embedding iunctus.
//!
//! Drivers are not `Send`; callers construct and use them on their owner thread. Validation
//! performs the checks of `input-schema`, including declared local path checks, but does not
//! fetch or verify remote content.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

use input_schema::{Severity, Validation};
use workflow_graph::{
    CancellationToken, Driver, DriverError, Engine, Execution, Executor, Inputs, Language,
    Observer, RunOptions,
};

/// How admission treats drivers without an enforceable input schema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    /// Keeps CLI-compatible behavior, including advisory or missing schemas.
    #[default]
    Relaxed,
    /// Requires a non-advisory schema and disables simulated input assumptions.
    Strict,
}

/// A driver's input and output description, obtained without starting a task.
#[derive(Clone, Debug, PartialEq)]
pub struct Description {
    pub language: Language,
    pub inputs: Option<Inputs>,
    pub outputs: Option<serde_json::Value>,
}

/// Accepted normalized inputs and their detailed diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct Admission {
    pub language: Language,
    pub inputs: Option<Inputs>,
    pub validation: Option<Validation>,
}

/// A completed engine execution together with the admission that preceded it.
#[derive(Debug)]
pub struct Executed {
    pub admission: Admission,
    pub execution: Execution,
}

/// Settings shared by execution with and without an observer.
pub struct ExecutionSettings {
    pub options: RunOptions,
    pub cancel: CancellationToken,
    pub simulated: bool,
    pub policy: Policy,
}

/// Why description or admission did not reach execution.
#[derive(Debug)]
pub enum ApplicationError {
    Driver(DriverError),
    MissingSchema {
        language: Language,
    },
    AdvisorySchema {
        language: Language,
        admission: Box<Admission>,
    },
    InvalidInputs {
        language: Language,
        admission: Box<Admission>,
    },
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Driver(error) => error.fmt(formatter),
            Self::MissingSchema { language } => {
                write!(formatter, "the {language} driver provides no input schema")
            }
            Self::AdvisorySchema { language, .. } => write!(
                formatter,
                "the {language} driver provides only an advisory input schema"
            ),
            Self::InvalidInputs { language, .. } => {
                write!(
                    formatter,
                    "the inputs of the {language} workflow are not valid"
                )
            }
        }
    }
}

impl std::error::Error for ApplicationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Driver(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DriverError> for ApplicationError {
    fn from(error: DriverError) -> Self {
        Self::Driver(error)
    }
}

/// Describes a configured driver without starting its workflow or any task.
pub fn describe(driver: &mut dyn Driver) -> Result<Description, ApplicationError> {
    Ok(Description {
        language: driver.language(),
        inputs: driver.inputs()?,
        outputs: driver.outputs()?,
    })
}

/// Validates a description without starting its workflow or applying values to its driver.
pub fn validate(
    description: Description,
    simulated: bool,
    policy: Policy,
) -> Result<Admission, ApplicationError> {
    checked(description, simulated, policy, input_schema::validate)
}

/// Validates already typed JSON without command-line coercion or default insertion.
pub fn validate_json(
    description: Description,
    simulated: bool,
    policy: Policy,
) -> Result<Admission, ApplicationError> {
    checked(description, simulated, policy, input_schema::validate_json)
}

fn checked(
    description: Description,
    simulated: bool,
    policy: Policy,
    validate: fn(&Inputs, bool) -> Validation,
) -> Result<Admission, ApplicationError> {
    let Description {
        language, inputs, ..
    } = description;
    let Some(mut inputs) = inputs else {
        return match policy {
            Policy::Relaxed => Ok(Admission {
                language,
                inputs: None,
                validation: None,
            }),
            Policy::Strict => Err(ApplicationError::MissingSchema { language }),
        };
    };
    let advisory = inputs.advisory;
    let strict = policy == Policy::Strict;
    if strict {
        inputs.assumes_values = false;
    }
    let validation = validate(&inputs, simulated && !strict);
    inputs.values = validation.values.clone();
    let admission = Admission {
        language,
        inputs: Some(inputs),
        validation: Some(validation),
    };
    if strict && advisory {
        return Err(ApplicationError::AdvisorySchema {
            language,
            admission: Box::new(admission),
        });
    }
    let invalid = admission.validation.as_ref().is_some_and(|validation| {
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    });
    if invalid {
        return Err(ApplicationError::InvalidInputs {
            language,
            admission: Box::new(admission),
        });
    }
    Ok(admission)
}

/// Describes, validates and applies normalized inputs without starting any task.
pub fn admit(
    driver: &mut dyn Driver,
    simulated: bool,
    policy: Policy,
) -> Result<Admission, ApplicationError> {
    let admission = validate(describe(driver)?, simulated, policy)?;
    if let Some(inputs) = &admission.inputs {
        driver.use_inputs(inputs.values.clone())?;
    }
    Ok(admission)
}

/// Admits and executes a configured driver and executor without an observer.
pub fn execute(
    workflow: &str,
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    options: RunOptions,
    cancel: CancellationToken,
    simulated: bool,
    policy: Policy,
) -> Result<Executed, ApplicationError> {
    run(
        workflow,
        driver,
        executor,
        ExecutionSettings {
            options,
            cancel,
            simulated,
            policy,
        },
        None,
    )
}

/// Admits and executes while forwarding engine events to `observer`.
pub fn execute_with(
    workflow: &str,
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    settings: ExecutionSettings,
    observer: &mut dyn Observer,
) -> Result<Executed, ApplicationError> {
    run(workflow, driver, executor, settings, Some(observer))
}

fn run(
    workflow: &str,
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    settings: ExecutionSettings,
    observer: Option<&mut dyn Observer>,
) -> Result<Executed, ApplicationError> {
    let ExecutionSettings {
        options,
        cancel,
        simulated,
        policy,
    } = settings;
    let admission = admit(driver, simulated, policy)?;
    let engine = Engine::new(executor).options(options).cancel(cancel);
    let execution = match observer {
        Some(observer) => engine.observer(observer).run(workflow, driver),
        None => engine.run(workflow, driver),
    };
    Ok(Executed {
        admission,
        execution,
    })
}
