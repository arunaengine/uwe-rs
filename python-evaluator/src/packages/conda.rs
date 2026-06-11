//! The conda packages that give a Python module, for the modules whose package has another
//! name, like `PIL` from `pillow`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Modules and the conda packages of conda-forge and bioconda that give them. A dotted name
/// is matched before its package, so `google.cloud.storage` differs from `google.protobuf`.
const PACKAGES: &[(&str, &str)] = &[
    ("Bio", "biopython"),
    ("Crypto", "pycryptodome"),
    ("HTSeq", "htseq"),
    ("Levenshtein", "levenshtein"),
    ("MySQLdb", "mysqlclient"),
    ("OpenSSL", "pyopenssl"),
    ("PIL", "pillow"),
    ("attr", "attrs"),
    ("bs4", "beautifulsoup4"),
    ("community", "python-louvain"),
    ("cv2", "opencv"),
    ("dateutil", "python-dateutil"),
    ("docx", "python-docx"),
    ("dotenv", "python-dotenv"),
    ("fitz", "pymupdf"),
    ("gi", "pygobject"),
    ("google.cloud.bigquery", "google-cloud-bigquery"),
    ("google.cloud.storage", "google-cloud-storage"),
    ("google.protobuf", "protobuf"),
    ("igraph", "python-igraph"),
    ("jwt", "pyjwt"),
    ("magic", "python-magic"),
    ("mpl_toolkits", "matplotlib-base"),
    ("osgeo", "gdal"),
    ("pptx", "python-pptx"),
    ("ruamel", "ruamel.yaml"),
    ("serial", "pyserial"),
    ("skimage", "scikit-image"),
    ("sklearn", "scikit-learn"),
    ("slugify", "python-slugify"),
    ("tables", "pytables"),
    ("upath", "universal_pathlib"),
    ("wget", "python-wget"),
    ("yaml", "pyyaml"),
    ("zmq", "pyzmq"),
];

/// The conda packages that give a module: from the table, else the name of its top package
/// in lower case with `_` as `-`, as conda names most Python packages.
pub fn conda_packages(module: &str) -> Vec<String> {
    let mut prefix = module;
    loop {
        if let Some((_, package)) = PACKAGES.iter().find(|(name, _)| *name == prefix) {
            return vec![(*package).to_owned()];
        }
        match prefix.rsplit_once('.') {
            Some((parent, _)) => prefix = parent,
            None => break,
        }
    }
    let top = module.split('.').next().unwrap_or(module);
    vec![top.to_lowercase().replace('_', "-")]
}

#[cfg(test)]
mod tests {
    use super::conda_packages;

    #[test]
    fn modules_map_to_conda_packages() {
        assert_eq!(conda_packages("PIL.Image"), ["pillow"]);
        assert_eq!(conda_packages("Bio.SeqIO"), ["biopython"]);
        assert_eq!(
            conda_packages("google.cloud.storage"),
            ["google-cloud-storage"]
        );
        assert_eq!(conda_packages("google.protobuf.json_format"), ["protobuf"]);
        assert_eq!(conda_packages("pyfaidx"), ["pyfaidx"]);
        assert_eq!(conda_packages("earth_osm.eo"), ["earth-osm"]);
    }
}
