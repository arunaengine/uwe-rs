//! nf-bactopia's `validateParameters`: the params against the JSON schema, then Bactopia's rules.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::{Level, log, param};
use crate::Context;
use crate::nf_schema::{entries, get, json, json_type, plain};

/// Params that the plugin never validates: options of Nextflow's command line and Bactopia's
/// own settings.
const IGNORED: &[&str] = &[
    "help",
    "help_all",
    "version",
    "workflow",
    "run_name",
    "publish_dir_mode",
    "schema_ignore_params",
    "wf",
    "profile",
    "resume",
];

const FAILED: &str = "\nValidation of pipeline parameters failed! Please correct to continue";

/// Validates the params; the result is the run type, or `success` for Bactopia Tools.
pub(super) fn parameters(tool: bool, context: &mut Context) -> Result<String, Error> {
    schema(context)?;
    let mut errors = Vec::new();
    let result = match tool {
        true => {
            tools(context, &mut errors);
            "success".to_owned()
        }
        false => pipeline(context, &mut errors)?,
    };
    if !errors.is_empty() {
        for error in &errors {
            log(Level::Error, error.clone());
        }
        log(Level::Error, FAILED);
    }
    Ok(result)
}

/// Checks the params that are set against the schema of `bactopia.parametersSchema`.
fn schema(context: &mut Context) -> Result<(), Error> {
    let file = match context.config.get(&["bactopia", "parametersSchema"]) {
        Some(Value::String(file)) => file.to_string(),
        _ => "nextflow_schema.json".into(),
    };
    let path = PathBuf::from(context.project_dir).join(file);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        thrown(
            "IOException",
            format!("Cannot read {}: {error}", path.display()),
        )
    })?;
    let schema: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        thrown(
            "IllegalArgumentException",
            format!("Invalid JSON schema {}: {error}", path.display()),
        )
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| thrown("IllegalArgumentException", error.to_string()))?;
    let mut ignored: Vec<String> = IGNORED.iter().map(|name| (*name).to_owned()).collect();
    if let Some(names) = param(context, "schema_ignore_params") {
        ignored.extend(names.split(',').map(|name| name.trim().to_owned()));
    }
    if let Some(Value::List(names)) = context.config.get(&["bactopia", "ignoreParams"]) {
        ignored.extend(names.borrow().items.iter().map(Value::to_string));
    }
    let instance = cleaned(&context.params, &ignored);
    let mut problems = Vec::new();
    for error in validator.iter_errors(&instance) {
        use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
        let value = error.instance();
        let message = match error.kind() {
            Kind::Type { kind } => {
                let expected: Vec<String> = match kind {
                    TypeKind::Single(kind) => vec![kind.to_string()],
                    TypeKind::Multiple(kinds) => {
                        kinds.iter().map(|kind| kind.to_string()).collect()
                    }
                };
                // The plugin checks types leniently: text takes any plain value, and numbers
                // and booleans take text that reads as one, like the params of the command line.
                let scalar = !value.is_array() && !value.is_object();
                let text = value.as_str().unwrap_or_default();
                let reads = |kind: &String| match kind.as_str() {
                    "string" => scalar,
                    "integer" => text.parse::<i64>().is_ok(),
                    "number" => text.parse::<f64>().is_ok(),
                    "boolean" => matches!(text, "true" | "false"),
                    _ => false,
                };
                if expected.iter().any(reads) {
                    continue;
                }
                format!(
                    "Value is [{}] but should be [{}]",
                    json_type(value),
                    expected.join(", ")
                )
            }
            _ => error.to_string(),
        };
        let name = error.instance_path().to_string();
        let name = name.trim_start_matches('/').replace('/', ".");
        problems.push(format!("    * --{name} ({}): {message}", plain(value)));
    }
    if !problems.is_empty() {
        log(Level::Error, "The following parameters are invalid:");
        for problem in problems {
            log(Level::Error, problem);
        }
        log(Level::Info, " ");
        log(Level::Error, FAILED);
    }
    Ok(())
}

/// The params as JSON without the ignored ones and those without a value; 0 stays.
fn cleaned(params: &Value, ignored: &[String]) -> serde_json::Value {
    let mut fields = serde_json::Map::new();
    for (name, value) in entries(params) {
        if ignored.contains(&name) {
            continue;
        }
        let value = match &value {
            Value::Null | Value::Boolean(false) => continue,
            Value::String(text) | Value::GString(text) if text.is_empty() => continue,
            Value::Map(_) => cleaned(&value, &[]),
            Value::MemoryUnit(_) | Value::Duration(_) | Value::Path(_) => {
                serde_json::Value::String(value.to_string())
            }
            other => json(other),
        };
        fields.insert(name, value);
    }
    serde_json::Value::Object(fields)
}

/// The rules of the Bactopia pipelines; the result is the run type the params ask for.
fn pipeline(context: &mut Context, errors: &mut Vec<String>) -> Result<String, Error> {
    let set = |name: &str| param(context, name).is_some();
    let (reads, sample) = (set("r1") && set("r2"), set("sample"));
    let runtype = if set("samples") {
        exists(context, "samples", false, errors);
        "is_fofn"
    } else if reads && set("ont") && set("short_polish") && sample {
        "short_polish"
    } else if reads && set("ont") && set("hybrid") && sample {
        "hybrid"
    } else if reads && set("se") {
        errors.push("Cannot use --r1, --r2, and --se together".into());
        ""
    } else if reads && set("ont") {
        errors.push(
            "Cannot use --r1, --r2, and --ont together, unless using --short_polish or --hybrid"
                .into(),
        );
        ""
    } else if set("ont") && set("se") {
        errors.push("Cannot use --ont and --se together".into());
        ""
    } else if reads && sample {
        "paired-end"
    } else if set("ont") && sample {
        "ont"
    } else if set("se") && sample {
        "single-end"
    } else if set("assembly") && sample {
        "assembly"
    } else if set("accessions") {
        exists(context, "accessions", false, errors);
        "is_accessions"
    } else if set("accession") {
        "is_accession"
    } else {
        errors.push("One or more required parameters (--r1, --r2, etc...) are missing, please check and try again.".into());
        ""
    };
    let files: &[&str] = match runtype {
        "paired-end" => &["r1", "r2"],
        "single-end" => &["se"],
        "hybrid" | "short_polish" => &["r1", "r2", "ont"],
        "ont" => &["ont"],
        "assembly" => &["assembly"],
        _ => &[],
    };
    for name in files {
        exists(context, name, true, errors);
    }
    if set("check_samples") && !set("samples") {
        errors.push(
            "To use --check_samples, you must also provide a FOFN to check using --samples.".into(),
        );
    }
    match get(&context.params, "max_downloads") {
        Some(Value::String(text)) => {
            let message = format!(
                "Cannot compare java.lang.String with value '{text}' and java.lang.Integer with value '10'"
            );
            return Err(thrown("IllegalArgumentException", message));
        }
        Some(Value::Number(count)) if count.to_f64() >= 10.0 => log(
            Level::Warn,
            format!(
                "Please be aware the value you have set for --max_downloads ({count}) may cause NCBI to temporarily block your IP address due to too many queries at once."
            ),
        ),
        _ => {}
    }
    if let Some(size) = get(&context.params, "genome_size").filter(|v| !matches!(v, Value::Null)) {
        let text = size.to_string();
        match text.parse::<i64>() {
            Err(_) => errors.push(format!("* --genome_size: \"{text}\" is not numeric.")),
            Ok(size) if size < 0 => {
                errors.push(format!(
                    "* --genome_size: \"{text}\" is not a positive integer."
                ));
            }
            Ok(_) => {}
        }
    }
    if get(&context.params, "adaptors").is_some() {
        for name in ["adaptors", "phix"] {
            if set(name) {
                exists(context, name, false, errors);
            }
        }
    }
    let workflow = workflow_name(context);
    if matches!(workflow.as_str(), "bactopia" | "staphopia") && set("use_bakta") {
        let found = database(context, "bakta_db", "bakta.db");
        if !found && !set("download_bakta") {
            errors.push("Bactopia requires --bakta_db to be set when using --use_bakta".into());
        }
    }
    if workflow == "teton" {
        if !set("kraken2_db") {
            errors.push("Teton requires '--kraken2_db' to be provided".into());
        }
        scrubber(context, "Teton", errors);
    }
    Ok(runtype.to_owned())
}

/// The rules of the Bactopia Tools.
fn tools(context: &mut Context, errors: &mut Vec<String>) {
    let set = |name: &str| param(context, name).is_some();
    match param(context, "bactopia") {
        None => errors.push("--bactopia is not set.".into()),
        Some(_) => exists(context, "bactopia", false, errors),
    }
    match (set("include"), set("exclude")) {
        (true, true) => errors.push("'--include' and '--exclude' cannot be used together".into()),
        (true, false) => exists(context, "include", false, errors),
        (false, true) => exists(context, "exclude", false, errors),
        (false, false) => {}
    }
    let mut missing: Vec<String> = Vec::new();
    let mut require = |name: &str, present: bool| {
        if !present {
            missing.push(format!("--{name}"));
        }
    };
    let workflow = workflow_name(context);
    let database_hint = |name: &str, file: &str, download: &str| {
        let present = database(context, name, file) || set(download);
        (
            format!("{name} (and --{download} if you want it to be automatically downloaded)"),
            present,
        )
    };
    match workflow.as_str() {
        "ariba" => require("ariba_db", set("ariba_db")),
        "bakta" => {
            let (name, present) = database_hint("bakta_db", "bakta.db", "download_bakta");
            require(&name, present);
        }
        "blastn" | "blastp" | "blastx" | "tblastn" | "tblastx" => {
            let name = format!("{workflow}_query");
            require(&name, set(&name));
        }
        "eggnog" => {
            let (name, present) = database_hint("eggnog_db", "eggnog.db", "download_eggnog");
            require(&name, present);
        }
        "fastani" => {
            let any = ["fastani_reference", "accession", "accessions", "species"]
                .iter()
                .any(|n| set(n));
            if !any && !set("fastani_pairwise") {
                errors.push("Either '--fastani_reference', '--accession', '--accessions', or '--species' is required unless using --fastani_pairwise".into());
            }
        }
        "gamma" => require("gamma_db", set("gamma_db")),
        "gtdb" => {
            let (name, present) = database_hint("gtdb", "metadata/metadata.txt", "download_gtdb");
            require(&name, present);
        }
        "kraken2" => require("kraken2_db", database(context, "kraken2_db", "hash.k2d")),
        "mashdist" => require("mash_sketch", set("mash_sketch")),
        "midas" => require("midas_db", database(context, "midas_db", "genome_info.txt")),
        "mykrobe" => require("mykrobe_species", set("mykrobe_species")),
        "pangenome" if set("scoary_traits") => exists(context, "scoary_traits", false, errors),
        "scoary" => require("scoary_traits", set("scoary_traits")),
        "scrubber" => scrubber(context, "Scrubber", errors),
        "snippy" => match (set("accession"), set("reference")) {
            (true, true) => {
                errors.push("'--accession' and '--reference' cannot be used together".into())
            }
            (false, false) => {
                errors.push("Either '--accession' and '--reference' is required".into())
            }
            _ => {}
        },
        "sylph" => require("sylph_db", set("sylph_db")),
        "traitar" => {
            let (name, present) = database_hint("traitar_db", "Pfam-A.hmm", "download_traitar");
            require(&name, present);
        }
        _ => {}
    }
    if !missing.is_empty() {
        errors.push(format!(
            "Required parameters are missing, please check: {}",
            missing.join(", ")
        ));
    }
}

/// The scrubber rule of Teton and the Scrubber tool.
fn scrubber(context: &mut Context, name: &str, errors: &mut Vec<String>) {
    let set = |param_name: &str| param(context, param_name).is_some();
    if set("deacon_db") || set("download_deacon") {
        if set("deacon_db") && !set("download_deacon") {
            exists(context, "deacon_db", false, errors);
        }
    } else if set("use_nohuman") {
        let found = database(context, "nohuman_db", "hash.k2d");
        if !found && !set("download_nohuman") {
            errors.push(
                "'--use_nohuman' requires '--nohuman_db' or '--download_nohuman' to be provided"
                    .into(),
            );
        }
    } else if !set("use_srascrubber") {
        errors.push(format!(
            "{name} requires '--deacon_db', '--use_nohuman', or '--use_srascrubber' to be provided"
        ));
    }
}

/// `params.workflow.name`.
fn workflow_name(context: &Context) -> String {
    let workflow = get(&context.params, "workflow").unwrap_or(Value::Null);
    get(&workflow, "name")
        .map(|name| name.to_string())
        .unwrap_or_default()
}

/// Whether a database param names a `.tar.gz` file or a folder with `file` in it.
fn database(context: &Context, name: &str, file: &str) -> bool {
    let Some(given) = param(context, name) else {
        return false;
    };
    if remote(&given) {
        return true;
    }
    let path = PathBuf::from(context.host.launch_dir()).join(&given);
    (given.ends_with(".tar.gz") && path.is_file()) || path.join(file).is_file()
}

fn remote(path: &str) -> bool {
    ["gs://", "s3://", "az://", "https://"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Checks that the file of a param exists and, for reads, that it is compressed with gzip.
fn exists(context: &Context, name: &str, gzip: bool, errors: &mut Vec<String>) {
    let Some(given) = param(context, name) else {
        return;
    };
    if remote(&given) {
        return;
    }
    let path = PathBuf::from(context.host.launch_dir()).join(&given);
    let Ok(mut file) = std::fs::File::open(&path) else {
        errors.push(format!(
            "* --{name}: Unable to find \"{given}\", please verify it exists."
        ));
        return;
    };
    let mut magic = [0u8; 2];
    if gzip && (file.read_exact(&mut magic).is_err() || magic != [0x1f, 0x8b]) {
        errors.push(format!(
            "* --{name}: Please verify \"{given}\" is compressed using GZIP"
        ));
    }
}

fn thrown(class: &'static str, message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new(class, message)),
        span: Default::default(),
    }
}
