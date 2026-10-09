//! Loads RDF format relationships and follows subclass and equivalent class links.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use cwl_parser::iri;
use cwl_parser::source::Remote;
use oxrdf::{NamedOrBlankNode, Term, Triple};

const LIMIT: u64 = 16 << 20;
type Relations = HashMap<String, HashSet<String>>;

#[derive(Debug, Default)]
pub(crate) struct Ontology {
    schemas: Vec<String>,
    remote: Remote,
    relations: OnceLock<Result<Relations, String>>,
}

impl Ontology {
    pub fn new(schemas: Vec<String>, remote: Remote) -> Self {
        Self {
            schemas,
            remote,
            relations: OnceLock::new(),
        }
    }

    pub fn matches(&self, format: &str, allowed: &[String]) -> Result<bool, String> {
        if allowed.iter().any(|allowed| allowed == format) {
            return Ok(true);
        }
        let relations = self.relations.get_or_init(|| self.load());
        let relations = relations.as_ref().map_err(Clone::clone)?;
        let mut pending = vec![format];
        let mut visited = HashSet::new();
        while let Some(format) = pending.pop() {
            if !visited.insert(format) {
                continue;
            }
            if allowed.iter().any(|allowed| allowed == format) {
                return Ok(true);
            }
            if let Some(parents) = relations.get(format) {
                pending.extend(parents.iter().map(String::as_str));
            }
        }
        Ok(false)
    }

    fn load(&self) -> Result<Relations, String> {
        let mut relations = Relations::new();
        for schema in &self.schemas {
            let mut read = || -> Result<(), String> {
                let path = match iri::file_path(schema) {
                    Some(path) => path,
                    None => {
                        let storage = self.remote.storage.blocking();
                        let metadata = storage
                            .metadata(schema, &self.remote.downloads)
                            .map_err(|error| error.to_string())?
                            .ok_or("the ontology does not exist")?;
                        if metadata.directory || metadata.size == 0 || metadata.size > LIMIT {
                            return Err(
                                "ontology downloads need a known size of at most 16 MiB".into()
                            );
                        }
                        storage
                            .fetch(schema, &self.remote.downloads)
                            .map_err(|error| error.to_string())?
                    }
                };
                if std::fs::metadata(&path)
                    .map_err(|error| error.to_string())?
                    .len()
                    > LIMIT
                {
                    return Err("the ontology exceeds 16 MiB".into());
                }
                let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
                parse(&bytes, schema, &mut relations)
            };
            read().map_err(|error| format!("cannot read ontology `{schema}`: {error}"))?;
        }
        Ok(relations)
    }
}

fn parse(bytes: &[u8], base: &str, relations: &mut Relations) -> Result<(), String> {
    let turtle = iri::split_fragment(base).0.ends_with(".ttl");
    if turtle {
        for triple in oxttl::TurtleParser::new()
            .with_base_iri(base)
            .map_err(|error| error.to_string())?
            .for_reader(bytes)
        {
            add(triple.map_err(|error| error.to_string())?, relations);
        }
    } else {
        for triple in oxrdfxml::RdfXmlParser::new()
            .with_base_iri(base)
            .map_err(|error| error.to_string())?
            .for_reader(bytes)
        {
            add(triple.map_err(|error| error.to_string())?, relations);
        }
    }
    Ok(())
}

fn add(triple: Triple, relations: &mut Relations) {
    let (NamedOrBlankNode::NamedNode(subject), Term::NamedNode(object)) =
        (triple.subject, triple.object)
    else {
        return;
    };
    let equivalent = triple.predicate.as_str() == "http://www.w3.org/2002/07/owl#equivalentClass";
    if equivalent || triple.predicate.as_str() == "http://www.w3.org/2000/01/rdf-schema#subClassOf"
    {
        relations
            .entry(subject.as_str().to_owned())
            .or_default()
            .insert(object.as_str().to_owned());
        if equivalent {
            relations
                .entry(object.as_str().to_owned())
                .or_default()
                .insert(subject.as_str().to_owned());
        }
    }
}

#[cfg(test)]
#[path = "ontology_tests.rs"]
mod tests;
