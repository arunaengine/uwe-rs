//! The formats of Files in a run: the prefixes of the formats of input values, and the
//! ontologies by which a format fits another one, which load when an exact match fails.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use cwl_parser::source::Remote;
use cwl_runtime::ontology::Ontology;
use serde_json::Value as Json;
use tool_definition::connection::format_fits;
use tool_definition::edam;

/// How the formats of a document match, as CWL matches them with its `$schemas`.
#[derive(Debug)]
pub struct FormatCheck {
    prefixes: BTreeMap<String, String>,
    ontology: Ontology,
    /// Whether the document names EDAM, whose terms match by the release built into uwe.
    edam: bool,
}

impl FormatCheck {
    /// The check of the formats of a document in the folder `base`, which reads remote
    /// ontologies through `remote`.
    pub fn new(formats: &wir::Formats, base: &Path, remote: Remote) -> FormatCheck {
        let (embedded, read): (Vec<&String>, Vec<&String>) =
            (formats.ontologies.iter()).partition(|location| location.as_str() == edam::ONTOLOGY);
        let schemas = (read.into_iter())
            .map(|location| match location.contains("://") {
                true => location.clone(),
                false => cwl_parser::iri::file_url(&base.join(location)),
            })
            .collect();
        FormatCheck {
            prefixes: formats.prefixes.clone(),
            ontology: Ontology::new(schemas, remote),
            edam: !embedded.is_empty(),
        }
    }

    /// Whether a File of the format `found` fits the `wanted` one by the ontologies: a
    /// subclass or an equivalent class of it, also through others.
    pub fn matches(&self, found: &str, wanted: &str) -> bool {
        if self.edam && edam_term(found) && edam_term(wanted) && format_fits(found, wanted) {
            return true;
        }
        let mut allowed = vec![wanted.to_owned()];
        // A format of another ontology fits when one of its parents there is an EDAM term
        // that fits by the embedded release.
        if self.edam && edam_term(wanted) && !edam_term(found) {
            let fitting = (edam::hierarchy().terms()).filter(|term| format_fits(term, wanted));
            allowed.extend(fitting.map(|term| format!("{EDAM}{term}")));
        }
        match self.ontology.matches(found, &allowed) {
            Ok(matched) => matched,
            Err(why) => {
                tracing::warn!(%why, "the ontologies of the formats cannot be read");
                false
            }
        }
    }

    /// Expands the prefixes of the formats of the Files in a value, as CWL expands those of
    /// a job.
    pub fn expand(&self, value: &mut Json) {
        match value {
            Json::Array(items) => items.iter_mut().for_each(|item| self.expand(item)),
            Json::Object(object) => {
                if object.get("class").and_then(Json::as_str) == Some("File")
                    && let Some(Json::String(format)) = object.get_mut("format")
                    && let Some((prefix, rest)) = format.split_once(':')
                    && let Some(iri) = self.prefixes.get(prefix)
                {
                    *format = format!("{iri}{rest}");
                }
                object.values_mut().for_each(|item| self.expand(item));
            }
            _ => {}
        }
    }
}

/// The namespace of the terms of EDAM.
const EDAM: &str = "http://edamontology.org/";

/// Whether a format is a term of the embedded EDAM release, by its IRI, `edam:` name or id.
fn edam_term(format: &str) -> bool {
    edam::hierarchy().contains(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(ontologies: &[&str]) -> FormatCheck {
        let formats = wir::Formats {
            prefixes: BTreeMap::new(),
            ontologies: ontologies.iter().map(|item| (*item).to_owned()).collect(),
        };
        FormatCheck::new(&formats, Path::new("/"), Remote::default())
    }

    #[test]
    fn matches_edam_kinds() {
        let iri = |id: &str| format!("http://edamontology.org/{id}");
        let embedded = check(&[edam::ONTOLOGY]);
        // FASTQ-sanger fits generic FASTQ, and FASTQ is a textual format.
        assert!(embedded.matches(&iri("format_1932"), &iri("format_1930")));
        assert!(embedded.matches(&iri("format_1930"), &iri("format_2330")));
        assert!(!embedded.matches(&iri("format_1930"), &iri("format_1932")));
        assert!(!embedded.matches(&iri("format_2572"), &iri("format_2330")));
        // Without EDAM among the ontologies only equal formats fit.
        assert!(!check(&[]).matches(&iri("format_1932"), &iri("format_1930")));
    }

    #[test]
    fn reaches_edam_parents() {
        let folder = tempfile::tempdir().expect("a folder");
        let rdf = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
  <rdf:Description rdf:about="http://example.org/fastq_lab">
    <rdfs:subClassOf rdf:resource="http://edamontology.org/format_1930"/>
  </rdf:Description>
</rdf:RDF>
"#;
        std::fs::write(folder.path().join("lab.rdf"), rdf).expect("an ontology");
        let formats = wir::Formats {
            prefixes: BTreeMap::new(),
            ontologies: vec![edam::ONTOLOGY.to_owned(), "lab.rdf".to_owned()],
        };
        let both = FormatCheck::new(&formats, folder.path(), Remote::default());
        let lab = "http://example.org/fastq_lab";
        // FASTQ is a textual format by EDAM, which the lab ontology does not say.
        assert!(both.matches(lab, "http://edamontology.org/format_1930"));
        assert!(both.matches(lab, "http://edamontology.org/format_2330"));
        assert!(!both.matches(lab, "http://edamontology.org/format_2572"));
    }
}
