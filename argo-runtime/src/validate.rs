//! The checks Argo makes when a workflow is submitted or a template is created
//! (workflow/validate/validate.go of v4.1.4), for what decides whether a workflow can run: the
//! names of templates, steps, tasks and parameters, the entrypoint, template types, references
//! to templates, the inputs that calls supply, the arguments of steps and tasks, hooks, and the
//! dependencies of DAG tasks. The messages are Argo's. Like Argo, a template is checked where
//! it is called and on its own, and templates of other documents only where they are called.
//!
//! The single rules are in the module `checks`.
//!
//! Left out: checks of Kubernetes fields (object name lengths, volume mounts, metrics, pod
//! garbage collection), cron schedules, and whether every tag names a known variable; a run
//! finds tags that do not resolve when it substitutes them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{Arguments, Template, TemplateRef, WorkflowSpec};
use argo_parser::{Node, Value as Yaml, decode};

use crate::checks::{
    Names, arguments, field_names, hooks, node_types, process_args, template_types,
};
use crate::library::{Document, Kind, Library};

/// What a call supplies to the inputs of the template it calls.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Supplied<'a> {
    /// Every input, as Argo's `FakeArguments` for templates that are checked on their own.
    Anything,
    Arguments(&'a Arguments),
}

/// Where template names resolve: the checked spec, or a WorkflowTemplate that a `templateRef`
/// reached.
#[derive(Debug)]
pub(crate) struct Base {
    pub label: String,
    pub path: PathBuf,
    pub templates: Vec<Node>,
}

impl Base {
    pub fn new(label: String, path: PathBuf, spec: &Node) -> Base {
        let templates = match spec.get("templates").map(|node| &node.value) {
            Some(Yaml::Sequence(templates)) => templates.clone(),
            _ => Vec::new(),
        };
        Base {
            label,
            path,
            templates,
        }
    }

    fn template(&self, name: &str) -> Option<&Node> {
        self.templates
            .iter()
            .find(|template| template.get("name").and_then(Node::as_str) == Some(name))
    }
}

/// What names the template of a call: a step, a task, a hook or the entrypoint.
struct Holder<'a> {
    template: &'a str,
    template_ref: Option<&'a TemplateRef>,
    inline: Option<&'a Template>,
}

impl<'a> Holder<'a> {
    fn named(template: &'a str) -> Self {
        Holder {
            template,
            template_ref: None,
            inline: None,
        }
    }
}

/// The state of one validation: the library, and the templates whose body was checked, as
/// Argo's `results`.
pub(crate) struct Check<'a> {
    library: &'a Library,
    checked: HashSet<(String, String)>,
    bases: Vec<((PathBuf, usize), Rc<Base>)>,
}

impl<'a> Check<'a> {
    pub fn new(library: &'a Library) -> Self {
        Check {
            library,
            checked: HashSet::new(),
            bases: Vec::new(),
        }
    }

    /// Checks a workflow spec whose templates are those of `base`. `entry` is the entrypoint
    /// and what the workflow supplies to it; without it the templates are only checked on
    /// their own, as for a WorkflowTemplate without an entrypoint. `allow_empty` lets workflow
    /// parameters be without a value, as when Argo lints a document or creates a template.
    pub fn spec(
        &mut self,
        spec: &WorkflowSpec,
        base: &Rc<Base>,
        entry: Option<(&str, Supplied)>,
        allow_empty: bool,
    ) -> Result<(), String> {
        let names: Vec<&str> = base
            .templates
            .iter()
            .map(|template| template.get("name").and_then(Node::as_str).unwrap_or(""))
            .collect();
        field_names(&names, Names::Field).map_err(|error| format!("spec.templates{error}"))?;
        arguments("spec.arguments.", &spec.arguments, allow_empty)?;
        if let Some((entrypoint, supplied)) = entry {
            self.holder(&Holder::named(entrypoint), base, supplied)?;
        }
        if !spec.on_exit.is_empty() {
            let supplied = Supplied::Arguments(&spec.arguments);
            self.holder(&Holder::named(&spec.on_exit), base, supplied)?;
        }
        hooks(&spec.hooks, "hooks")?;
        for name in names {
            self.holder(&Holder::named(name), base, Supplied::Anything)
                .map_err(|error| format!("templates.{name} {error}"))?;
        }
        Ok(())
    }

    fn base_of(&mut self, document: &Rc<Document>) -> Rc<Base> {
        let key = (document.path.clone(), document.index);
        if let Some((_, base)) = self.bases.iter().find(|(known, _)| known == &key) {
            return Rc::clone(base);
        }
        let empty = Node::new(Yaml::Mapping(Vec::new()), Default::default());
        let spec = document.spec().unwrap_or(&empty);
        let base = Rc::new(Base::new(document.label(), document.path.clone(), spec));
        self.bases.push((key, Rc::clone(&base)));
        base
    }

    /// Finds the template of a call and checks it, as Argo's `validateTemplateHolder`.
    fn holder(
        &mut self,
        holder: &Holder,
        base: &Rc<Base>,
        supplied: Supplied,
    ) -> Result<(), String> {
        let (base, node, template) = if let Some(reference) = holder.template_ref {
            if !holder.template.is_empty() {
                return Err("template name cannot be specified with templateRef".to_owned());
            }
            if reference.name.is_empty() {
                return Err("resource name is required".to_owned());
            }
            if reference.template.is_empty() {
                return Err("template name is required".to_owned());
            }
            // A reference that a tag chooses is resolved when the workflow runs.
            if reference.name.contains("{{") || reference.template.contains("{{") {
                return Ok(());
            }
            let kind = if reference.cluster_scope {
                Kind::ClusterWorkflowTemplate
            } else {
                Kind::WorkflowTemplate
            };
            let document = self
                .library
                .find_template(kind, &reference.name, &reference.template, &base.path)
                .ok_or_else(|| not_found(kind, &reference.name))?;
            let base = self.base_of(&document);
            let Some(node) = base.template(&reference.template).cloned() else {
                return Err(format!(
                    "template reference {}.{} not found",
                    reference.name, reference.template
                ));
            };
            let template = decode_template(&node)?;
            (base, Some(node), template)
        } else if let Some(inline) = holder.inline {
            (Rc::clone(base), None, inline.clone())
        } else if !holder.template.is_empty() {
            let Some(node) = base.template(holder.template).cloned() else {
                return Err(format!("template name '{}' undefined", holder.template));
            };
            let template = decode_template(&node)?;
            (Rc::clone(base), Some(node), template)
        } else {
            return Err("failed to get a template".to_owned());
        };
        let types = match &node {
            Some(node) => node_types(node),
            None => template_types(&template),
        };
        if types == 0 {
            return Err(format!("template '{}' type is unknown", template.name));
        }
        if let Some(strategy) = &template.retry_strategy
            && !matches!(
                strategy.retry_policy.as_str(),
                "" | "Always" | "OnError" | "OnFailure" | "OnTransientError"
            )
        {
            return Err(format!(
                "{} is not a valid RetryPolicy",
                strategy.retry_policy
            ));
        }
        self.template(&template, node.as_ref(), &base, supplied)
    }

    /// Checks a template: its type, its inputs against what the call supplies, and once per
    /// template its body, as Argo's `validateTemplate`.
    fn template(
        &mut self,
        template: &Template,
        node: Option<&Node>,
        base: &Rc<Base>,
        supplied: Supplied,
    ) -> Result<(), String> {
        let name = &template.name;
        let types = match node {
            Some(node) => node_types(node),
            None => template_types(template),
        };
        if types > 1 {
            return Err(format!(
                "templates.{name} multiple template types specified. choose one of: container, containerSet, steps, script, resource, dag, suspend, template, template ref"
            ));
        }
        let parameters: Vec<&str> = template
            .inputs
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        field_names(&parameters, Names::Parameter)
            .map_err(|error| format!("templates.{name}.inputs.parameters{error}"))?;
        let artifacts: Vec<&str> = template
            .inputs
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        field_names(&artifacts, Names::Parameter)
            .map_err(|error| format!("templates.{name}.inputs.artifacts{error}"))?;
        process_args(template, supplied).map_err(|error| format!("templates.{name} {error}"))?;
        let leaf = template.steps.is_empty() && template.dag.is_none();
        if let Some(timeout) = &template.timeout {
            if !leaf {
                let kind = if template.dag.is_some() {
                    "DAG"
                } else {
                    "Steps"
                };
                return Err(format!("{kind} template doesn't support timeout field"));
            }
            if timeout.0.trim().parse::<i64>().is_ok() {
                return Err(format!("{name} has invalid duration format in timeout"));
            }
        }
        let id = match node {
            Some(_) => name.clone(),
            None => format!("inline {template:?}"),
        };
        if !self.checked.insert((base.label.clone(), id)) {
            return Ok(());
        }
        if !leaf && template.active_deadline_seconds.is_some() {
            return Err(format!(
                "templates.{name}.activeDeadlineSeconds is only valid for leaf templates"
            ));
        }
        Ok(())
    }
}

fn decode_template(node: &Node) -> Result<Template, String> {
    let name = node.get("name").and_then(Node::as_str).unwrap_or("");
    decode::<Template>(node)
        .map(|(template, _)| template)
        .map_err(|error| format!("templates.{name}: {error}"))
}

/// Argo's message for a WorkflowTemplate or ClusterWorkflowTemplate that is not there.
pub(crate) fn not_found(kind: Kind, name: &str) -> String {
    match kind {
        Kind::ClusterWorkflowTemplate => {
            format!("couldn't find cluster workflow template \"{name}\"")
        }
        _ => format!("couldn't find workflow template \"{name}\""),
    }
}
