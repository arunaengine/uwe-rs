//! The file table of a BIDS dataset: BIDS-shaped file names with their entities, after the
//! ignore rules, as the plugin's libBIDS.sh step lists them, done in Rust.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::regex::Pattern;

/// The BIDS entities: the key in file names and the column name, in column order.
pub(super) const ENTITIES: [(&str, &str); 35] = [
    ("sub", "subject"),
    ("tpl", "template"),
    ("ses", "session"),
    ("cohort", "cohort"),
    ("sample", "sample"),
    ("task", "task"),
    ("tracksys", "tracksys"),
    ("acq", "acquisition"),
    ("nuc", "nucleus"),
    ("voi", "volume"),
    ("ce", "ceagent"),
    ("trc", "tracer"),
    ("stain", "stain"),
    ("rec", "reconstruction"),
    ("dir", "direction"),
    ("run", "run"),
    ("mod", "modality"),
    ("echo", "echo"),
    ("flip", "flip"),
    ("inv", "inversion"),
    ("mt", "mtransfer"),
    ("part", "part"),
    ("proc", "processing"),
    ("hemi", "hemisphere"),
    ("space", "space"),
    ("split", "split"),
    ("recording", "recording"),
    ("chunk", "chunk"),
    ("atlas", "atlas"),
    ("seg", "segmentation"),
    ("scale", "scale"),
    ("res", "resolution"),
    ("den", "density"),
    ("label", "label"),
    ("desc", "description"),
];

const SUFFIXES: &str = "2PE ADC BF Chimap CARS CONF DIC DF FA FLAIR FLASH FLUO IRT1 M0map MEGRE MESE \
    MP2RAGE MPE MPM MTR MTRmap MTS MTVmap MTsat MWFmap NLO OCT PC PD PDT2 PDmap PDw PLI R1map \
    R2map R2starmap RB1COR RB1map S0map SEM SPIM SR T1map T1rho T1w T2map T2star T2starmap \
    T2starw T2w TB1AFI TB1DAM TB1EPI TB1RFM TB1SRGE TB1TFL TB1map TEM UNIT1 VFA angio asl \
    aslcontext asllabeling beh blood bold cbv channels colFA coordsystem defacemask description \
    descriptions dseg dwi eeg electrodes emg epi events expADC fieldmap headshape XPCT ieeg \
    inplaneT1 inplaneT2 m0scan magnitude magnitude1 magnitude2 markers mask meg motion mrsi \
    mrsref nirs noRF optodes pet phase phase1 phase2 phasediff photo physio physioevents \
    probseg sbref scans sessions stim svs trace uCT unloc";

const EXTENSIONS: [&str; 41] = [
    "ave",
    "bdf",
    "bval",
    "bvec",
    "chn",
    "con",
    "dat",
    "ds",
    "dlabel.nii",
    "edf",
    "eeg",
    "fdt",
    "fif",
    "jpg",
    "json",
    "kdf",
    "label.gii",
    "md",
    "mefd",
    "mhd",
    "mrk",
    "ome.zarr",
    "nii",
    "nii.gz",
    "nwb",
    "ome.btf",
    "ome.tif",
    "png",
    "pos",
    "raw",
    "rst",
    "set",
    "snirf",
    "sqd",
    "tif",
    "trg",
    "tsv",
    "tsv.gz",
    "txt",
    "vhdr",
    "vmrk",
];

/// The ignore patterns that apply before a dataset's `.bidsignore`.
const DEFAULT_IGNORES: [&str; 6] = [".git**", ".*", "sourcedata/", "code/", "stimuli/", "log/"];

/// One listed file.
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub path: PathBuf,
    /// The path inside the dataset, with `/` between parts.
    pub relative: String,
    /// The entities that the name sets, by column name, each as `key-value`.
    /// An entity that BIDS does not know keeps its key as the column, like `bp`.
    pub entities: Vec<(String, String)>,
    pub suffix: String,
    /// The name after its first `.`, like `nii.gz`.
    pub extension: String,
}

impl Row {
    /// The value of an entity by column name or key, `NA` when the name does not set it.
    pub(super) fn entity(&self, name: &str) -> &str {
        let column = column(name).unwrap_or(name);
        (self.entities.iter())
            .find(|(known, _)| *known == column)
            .map_or("NA", |(_, value)| value.as_str())
    }
}

/// The column of an entity key or column name.
pub(super) fn column(name: &str) -> Option<&'static str> {
    ENTITIES
        .iter()
        .find(|(key, column)| *key == name || *column == name)
        .map(|(_, column)| *column)
}

/// The listed files of a dataset in path order, with the ignore rules when `ignore` is set.
pub(super) fn table(root: &Path, ignore: bool, defaults: bool) -> Vec<Row> {
    let mut rules = Vec::new();
    if ignore {
        if defaults {
            rules.extend(DEFAULT_IGNORES.iter().filter_map(|line| Rule::parse(line)));
        }
        let own = std::fs::read_to_string(root.join(".bidsignore")).unwrap_or_default();
        rules.extend(own.lines().filter_map(Rule::parse));
    }
    let mut rows = Vec::new();
    walk(root, "", &rules, &mut rows);
    rows.sort_by(|a, b| a.relative.cmp(&b.relative));
    rows
}

fn walk(directory: &Path, prefix: &str, rules: &[Rule], rows: &mut Vec<Row>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}{name}");
        let path = entry.path();
        let folder = path.is_dir();
        if ignored(rules, &relative, folder) {
            continue;
        }
        if folder {
            walk(&path, &format!("{relative}/"), rules, rows);
        } else if let Some(row) = parse(&path, &relative) {
            rows.push(row);
        }
    }
}

/// Reads a BIDS file name like `sub-01_dir-AP_dwi.nii.gz`.
fn parse(path: &Path, relative: &str) -> Option<Row> {
    let name = relative.rsplit('/').next()?;
    let (stem, extension) = name.split_once('.')?;
    if !EXTENSIONS.contains(&extension) {
        return None;
    }
    let (head, suffix) = stem.rsplit_once('_')?;
    if !SUFFIXES.split_whitespace().any(|known| known == suffix) {
        return None;
    }
    let mut entities: Vec<(String, String)> = Vec::new();
    for part in head.split('_') {
        let Some((key, _)) = part.split_once('-') else {
            continue;
        };
        let column = column(key).unwrap_or(key).to_owned();
        if !entities.iter().any(|(known, _)| *known == column) {
            entities.push((column, part.to_owned()));
        }
    }
    let place = |column: &str| {
        ENTITIES
            .iter()
            .position(|(_, c)| *c == column)
            .unwrap_or(ENTITIES.len())
    };
    entities.sort_by_key(|(column, _)| place(column));
    Some(Row {
        path: path.to_owned(),
        relative: relative.to_owned(),
        entities,
        suffix: suffix.to_owned(),
        extension: extension.to_owned(),
    })
}

/// A line of `.gitignore` syntax.
struct Rule {
    pattern: Pattern,
    negated: bool,
    folder_only: bool,
}

impl Rule {
    fn parse(line: &str) -> Option<Rule> {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (negated, line) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line.strip_prefix('\\').unwrap_or(line)),
        };
        let (folder_only, line) = match line.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let anchored = line.contains('/');
        let line = line.strip_prefix('/').unwrap_or(line);
        let body = translate(line);
        let source = match anchored {
            true => format!("^{body}$"),
            false => format!("^(?:.*/)?{body}$"),
        };
        let pattern = Pattern::new(&source).ok()?;
        Some(Rule {
            pattern,
            negated,
            folder_only,
        })
    }
}

/// A glob as a regular expression: `*` and `?` stay within a part, `**` crosses parts.
fn translate(glob: &str) -> String {
    let characters: Vec<char> = glob.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let rest: String = characters[index..].iter().collect();
        if rest.starts_with("**/") && (index == 0 || characters[index - 1] == '/') {
            out.push_str("(?:.*/)?");
            index += 3;
            continue;
        }
        if rest == "**" && (index == 0 || characters[index - 1] == '/') {
            out.push_str(".*");
            break;
        }
        match character {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => match characters[index..].iter().position(|c| *c == ']') {
                Some(end) => {
                    let class: String = characters[index + 1..index + end].iter().collect();
                    let class = class
                        .strip_prefix('!')
                        .map_or(class.clone(), |rest| format!("^{rest}"));
                    out.push_str(&format!("[{class}]"));
                    index += end;
                }
                None => out.push_str("\\["),
            },
            '\\' if index + 1 < characters.len() => {
                index += 1;
                out.push_str(&escape(characters[index]));
            }
            other => out.push_str(&escape(other)),
        }
        index += 1;
    }
    out
}

fn escape(character: char) -> String {
    match character.is_alphanumeric() || character == '/' || character == '_' || character == '-' {
        true => character.to_string(),
        false => format!("\\{character}"),
    }
}

/// Whether the last rule that matches a path ignores it.
fn ignored(rules: &[Rule], relative: &str, folder: bool) -> bool {
    let mut ignored = false;
    for rule in rules {
        if rule.folder_only && !folder {
            continue;
        }
        if rule.pattern.matches(relative).unwrap_or(false) {
            ignored = !rule.negated;
        }
    }
    ignored
}

#[cfg(test)]
mod tests {
    use super::{Rule, ignored, parse};

    #[test]
    fn reads_bids_names() {
        let row = parse("/d/x".as_ref(), "sub-01/dwi/sub-01_dir-AP_dwi.nii.gz").unwrap();
        assert_eq!(
            (row.suffix.as_str(), row.extension.as_str()),
            ("dwi", "nii.gz")
        );
        assert_eq!(
            (row.entity("subject"), row.entity("dir"), row.entity("run")),
            ("sub-01", "dir-AP", "NA")
        );
        assert!(parse("/d/x".as_ref(), "README").is_none());
        assert!(parse("/d/x".as_ref(), "dataset_description.json").is_some());
    }

    #[test]
    fn follows_ignore_rules() {
        let rules: Vec<Rule> = [".*", "code/", "sub-01/dwi/*.bvec", "!keep.txt", "**/tmp"]
            .iter()
            .filter_map(|line| Rule::parse(line))
            .collect();
        assert!(ignored(&rules, ".git", true));
        assert!(ignored(&rules, "code", true));
        assert!(!ignored(&rules, "code", false));
        assert!(ignored(&rules, "sub-01/dwi/sub-01_dwi.bvec", false));
        assert!(!ignored(&rules, "sub-02/dwi/sub-01_dwi.bvec", false));
        assert!(ignored(&rules, "a/b/tmp", true));
    }
}
