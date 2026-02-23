//! The CWL v1.2 vocabulary, its namespaces and the Schema Salad annotations of its fields.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::iri::{self, Parts};

pub const CWL: &str = "https://w3id.org/cwl/cwl#";
const SALAD: &str = "https://w3id.org/cwl/salad#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// Terms in the CWL namespace, taken from the context that cwltool builds for v1.2.
const CWL_TERMS: &str = "CWLType CWLVersion CommandInputArraySchema CommandInputEnumSchema \
    CommandInputParameter CommandInputRecordField CommandInputRecordSchema CommandInputSchema \
    CommandLineBindable CommandLineBinding CommandLineTool CommandOutputArraySchema \
    CommandOutputBinding CommandOutputEnumSchema CommandOutputParameter CommandOutputRecordField \
    CommandOutputRecordSchema Directory Dirent DockerRequirement EnvVarRequirement EnvironmentDef \
    Expression ExpressionPlaceholder ExpressionTool ExpressionToolOutputParameter FieldBase File \
    IOSchema Identified InitialWorkDirRequirement InlineJavascriptRequirement \
    InplaceUpdateRequirement InputArraySchema InputBinding InputEnumSchema InputFormat \
    InputParameter InputRecordField InputRecordSchema InputSchema Labeled LinkMergeMethod \
    LoadContents LoadListingEnum LoadListingRequirement MultipleInputFeatureRequirement \
    NetworkAccess Operation OperationInputParameter OperationOutputParameter OutputArraySchema \
    OutputEnumSchema OutputFormat OutputParameter OutputRecordField OutputRecordSchema \
    OutputSchema Parameter PickValueMethod Process ProcessRequirement ResourceRequirement \
    ScatterFeatureRequirement ScatterMethod SchemaDefRequirement SecondaryFileSchema \
    ShellCommandRequirement Sink SoftwarePackage SoftwareRequirement \
    StepInputExpressionRequirement SubworkflowFeatureRequirement ToolTimeLimit WorkReuse Workflow \
    WorkflowInputParameter WorkflowOutputParameter WorkflowStep WorkflowStepInput \
    WorkflowStepOutput arguments baseCommand basename cwlVersion draft-2 draft-3 draft-3.dev1 \
    draft-3.dev2 draft-3.dev3 draft-3.dev4 draft-3.dev5 draft-4.dev1 draft-4.dev2 draft-4.dev3 \
    entry entryname format hints in inputBinding inputs linkMerge listing loadContents \
    loadListing out outputBinding outputSource outputs path pickValue requirements run scatter \
    scatterMethod secondaryFiles source stderr stdin stdout v1.0 v1.0.dev4 v1.1 v1.1.0-dev1 v1.2 \
    v1.2.0-dev1 v1.2.0-dev2 v1.2.0-dev3 v1.2.0-dev4 v1.2.0-dev5 valueFrom when";

/// Terms whose IRI is nested below a record or enum of the CWL namespace.
const NESTED_TERMS: &str = "Directory_class File_class all_non_null checksum contents coresMax \
    coresMin deep_listing dirname dockerFile dockerImageId dockerImport dockerLoad \
    dockerOutputDirectory dockerPull dotproduct enableReuse envDef envName envValue expression \
    expressionLib first_non_null flat_crossproduct glob inplaceUpdate intent itemSeparator \
    merge_flattened merge_nested nameext nameroot nested_crossproduct networkAccess no_listing \
    outdirMax outdirMin outputEval package packages pattern permanentFailCodes position prefix \
    ramMax ramMin required separate shallow_listing shellQuote size specs steps streamable \
    successCodes temporaryFailCodes the_only_non_null timelimit tmpdirMax tmpdirMin types \
    version writable";

const SALAD_TERMS: &str = "Any ArraySchema Documented EnumSchema PrimitiveType RecordField \
    RecordSchema array default enum fields items null record symbols type";

const XSD_TERMS: &str = "boolean double float int long string";

/// Vocabulary terms and, for most, the IRI that shortens back to the term.
struct Vocabulary {
    terms: HashMap<&'static str, Option<String>>,
    reverse: HashMap<String, &'static str>,
}

fn vocabulary() -> &'static Vocabulary {
    static VOCABULARY: OnceLock<Vocabulary> = OnceLock::new();
    VOCABULARY.get_or_init(|| {
        let mut terms = HashMap::new();
        for (list, namespace) in [(CWL_TERMS, CWL), (SALAD_TERMS, SALAD), (XSD_TERMS, XSD)] {
            for term in list.split_whitespace() {
                terms.insert(term, Some(format!("{namespace}{term}")));
            }
        }
        for term in NESTED_TERMS.split_whitespace() {
            terms.insert(term, None);
        }
        terms.insert("label", Some(format!("{RDFS}label")));
        terms.insert("doc", Some(format!("{RDFS}comment")));
        terms.insert("class", None);
        terms.insert("location", None);
        let reverse = terms
            .iter()
            .filter_map(|(term, iri)| Some((iri.clone()?, *term)))
            .collect();
        Vocabulary { terms, reverse }
    })
}

/// Whether the text is a term of the CWL vocabulary, such as `File` or `dotproduct`.
pub fn is_term(text: &str) -> bool {
    vocabulary().terms.contains_key(text)
}

/// The term that an expanded IRI stands for, such as `string` for the XML Schema string.
pub fn term_of(iri: &str) -> Option<&'static str> {
    vocabulary().reverse.get(iri).copied()
}

/// Namespace prefixes in scope; the CWL schema declares the first six.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespaces {
    pairs: Vec<(String, String)>,
}

impl Default for Namespaces {
    fn default() -> Self {
        let schema = [
            ("cwl", CWL),
            ("sld", SALAD),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("rdfs", RDFS),
            ("xsd", XSD),
            ("dct", "http://purl.org/dc/terms/"),
        ];
        let pairs = schema
            .iter()
            .map(|(prefix, iri)| ((*prefix).to_owned(), (*iri).to_owned()))
            .collect();
        Self { pairs }
    }
}

impl Namespaces {
    /// These namespaces with more declarations, which take precedence.
    pub fn extend(&self, declared: Vec<(String, String)>) -> Self {
        let mut pairs = self.pairs.clone();
        pairs.extend(declared);
        Self { pairs }
    }

    fn lookup(&self, prefix: &str) -> Option<&str> {
        let found = self.pairs.iter().rev().find(|(name, _)| name == prefix);
        found.map(|(_, iri)| iri.as_str())
    }

    /// Replaces a declared `prefix:` at the start of the text with its IRI.
    pub fn expand_prefix(&self, text: &str) -> String {
        if let Some((prefix, rest)) = text.split_once(':')
            && let Some(iri) = self.lookup(prefix)
        {
            return format!("{iri}{rest}");
        }
        text.to_owned()
    }

    /// Expands a reference as Schema Salad's `expand_url` does.
    pub fn expand(&self, text: &str, base: &str, rule: Expansion) -> String {
        if text == "@id" || text == "@type" || text.starts_with("_:") {
            return text.to_owned();
        }
        if rule.term && is_term(text) {
            return text.to_owned();
        }
        let url = self.expand_prefix(text);
        let parts = Parts::parse(&url);
        let fetchable = matches!(parts.scheme, Some("file" | "http" | "https" | "mailto"));
        let has_fragment = parts.fragment.is_some_and(|fragment| !fragment.is_empty());
        let expanded = if fetchable || url.starts_with("$(") || url.starts_with("${") {
            url
        } else if rule.identifier && !has_fragment {
            scope_under(base, parts.path)
        } else if rule.scoped && !has_fragment {
            url
        } else {
            iri::join(base, &url)
        };
        if rule.term
            && let Some(term) = term_of(&expanded)
        {
            return term.to_owned();
        }
        expanded
    }
}

/// How `expand` treats a reference, as the annotations of its field select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// An identifier, placed in the fragment scope of its parent.
    pub identifier: bool,
    /// A vocabulary field: terms stay terms and known IRIs shorten to terms.
    pub term: bool,
    /// A reference searched in enclosing scopes later, so without a fragment it stays as written.
    pub scoped: bool,
}

impl Expansion {
    pub const LINK: Self = Self {
        identifier: false,
        term: false,
        scoped: false,
    };
    pub const IDENTIFIER: Self = Self {
        identifier: true,
        ..Self::LINK
    };

    /// The rule for a link field of the given name.
    pub fn field(name: &str) -> Self {
        Self {
            identifier: false,
            term: is_vocabulary(name),
            scoped: reference_scope(name).is_some(),
        }
    }
}

/// Appends a relative identifier to the fragment of the base, or makes it the fragment.
fn scope_under(base: &str, path: &str) -> String {
    let parts = Parts::parse(base);
    let fragment = match parts.fragment {
        Some(fragment) if !fragment.is_empty() => format!("{fragment}/{path}"),
        _ => path.to_owned(),
    };
    let base_path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path
    };
    let fragment = (!fragment.is_empty()).then_some(fragment.as_str());
    Parts {
        path: base_path,
        ..parts
    }
    .compose(fragment)
}

/// How an identifier map turns into a list: the key goes into `subject`, and
/// a value that is not a map goes into `predicate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapForm {
    pub subject: &'static str,
    pub predicate: Option<&'static str>,
}

/// The identifier map annotation (`mapSubject`, `mapPredicate`) of a field.
pub fn map_form(field: &str) -> Option<MapForm> {
    let (subject, predicate) = match field {
        "inputs" | "outputs" => ("id", Some("type")),
        "requirements" | "hints" => ("class", None),
        "steps" => ("id", None),
        "in" => ("id", Some("source")),
        "fields" => ("name", Some("type")),
        "envDef" => ("envName", Some("envValue")),
        "packages" => ("package", Some("specs")),
        _ => return None,
    };
    Some(MapForm { subject, predicate })
}

/// Fields that name the object they belong to, in the order Salad checks them.
pub const IDENTIFIERS: [&str; 2] = ["id", "name"];

/// Fields whose values are resolved as links (`_type: @id` or `@vocab`).
pub fn is_link(field: &str) -> bool {
    matches!(
        field,
        "$schemas"
            | "class"
            | "cwlVersion"
            | "format"
            | "intent"
            | "items"
            | "location"
            | "out"
            | "outputSource"
            | "path"
            | "run"
            | "scatter"
            | "scatterMethod"
            | "source"
            | "specs"
            | "symbols"
            | "type"
    )
}

/// Fields whose values are vocabulary terms when they can be.
pub fn is_vocabulary(field: &str) -> bool {
    matches!(
        field,
        "class" | "cwlVersion" | "items" | "scatterMethod" | "type"
    )
}

/// Fields whose links name new objects rather than point at existing ones.
pub fn is_identity(field: &str) -> bool {
    matches!(
        field,
        "format" | "id" | "intent" | "name" | "out" | "symbols"
    )
}

/// How many scope levels a scoped reference drops before searching upwards (`refScope`).
pub fn reference_scope(field: &str) -> Option<usize> {
    match field {
        "items" | "source" | "type" => Some(2),
        "outputSource" | "scatter" => Some(0),
        _ => None,
    }
}

/// Fields whose links are not checked (`noLinkCheck`).
pub fn skips_check(field: &str) -> bool {
    matches!(field, "default" | "hints" | "specs")
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
