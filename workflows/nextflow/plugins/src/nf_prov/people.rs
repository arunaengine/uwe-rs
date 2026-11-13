//! The people of the RO-Crate of nf-prov: the agent who ran the workflow, its organization,
//! their contact points, and the authors that the manifest names.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::wrroc::{Builder, reference};
use crate::report_format::Json;

impl Builder<'_> {
    /// The agent, its organization, their contact points and the authors of the manifest.
    pub(super) fn people(&self) -> (Option<String>, People) {
        let mut people = People::default();
        let setting = |scope: &str, key: &str| self.options[scope][key].as_str().map(str::to_owned);
        let organization = self.options["organization"]
            .is_object()
            .then(|| setting("organization", "ror").unwrap_or_else(|| "organization-1".to_owned()));
        let agent = self.options["agent"]
            .is_object()
            .then(|| setting("agent", "orcid").unwrap_or_else(|| "agent-1".to_owned()));
        let mut contacts = Vec::new();
        let mut contact = |scope: &str, url: Option<String>| {
            let email = setting(scope, "email");
            let phone = setting(scope, "phone");
            let id = (email.as_ref().map(|email| format!("mailto:{email}"))).or(phone.clone())?;
            contacts.push(Json::object([
                ("@id", Json::from(id.as_str())),
                ("@type", "ContactPoint".into()),
                ("contactType", setting(scope, "contactType").into()),
                ("email", email.into()),
                ("phone", phone.into()),
                ("url", url.into()),
            ]));
            Some(reference(&id))
        };
        if let Some(id) = &agent {
            let point = contact("agent", setting("agent", "orcid"));
            people.entities.push(Json::object([
                ("@id", Json::from(id.as_str())),
                ("@type", "Person".into()),
                ("name", setting("agent", "name").into()),
                ("contactPoint", point.into()),
                ("affiliation", organization.as_deref().map(reference).into()),
            ]));
        }
        if let Some(id) = &organization {
            let point = contact("organization", setting("organization", "ror"));
            people.entities.push(Json::object([
                ("@id", Json::from(id.as_str())),
                ("@type", "Organization".into()),
                ("name", setting("organization", "name").into()),
                ("url", setting("organization", "ror").into()),
                ("contactPoint", point.into()),
            ]));
        }
        let contributors = self.run.start.manifest["contributors"].as_array();
        for person in contributors.into_iter().flatten() {
            let roles = person["contribution"].as_array();
            if !roles.is_some_and(|roles| roles.iter().any(|role| role == "author")) {
                continue;
            }
            let name = person["name"].as_str().unwrap_or_default();
            let id = (person["orcid"].as_str().map(str::to_owned)).unwrap_or_else(|| {
                format!("#{}", name.split_whitespace().collect::<Vec<_>>().join("_"))
            });
            if Some(&id) != agent.as_ref() {
                people.entities.push(Json::object([
                    ("@id", Json::from(id.as_str())),
                    ("@type", "Person".into()),
                    ("name", name.into()),
                    ("email", Json::from(&person["email"])),
                ]));
            }
            people.creators.push(id);
        }
        people.entities.extend(contacts);
        people.organization = organization;
        (agent, people)
    }
}

#[derive(Default)]
pub(super) struct People {
    pub(super) entities: Vec<Json>,
    /// The authors of the manifest, by id.
    pub(super) creators: Vec<String>,
    pub(super) organization: Option<String>,
}
