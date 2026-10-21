//! The PyPI packages that give a Python module, for the modules whose package has another
//! name, like `PIL` from `pillow`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Modules and the PyPI packages that give them. A dotted name is matched before its
/// package, so `google.cloud.storage` differs from `google.protobuf`.
const PACKAGES: &[(&str, &str)] = &[
    ("Bio", "biopython"),
    ("Crypto", "pycryptodome"),
    ("MySQLdb", "mysqlclient"),
    ("OpenSSL", "pyopenssl"),
    ("PIL", "pillow"),
    ("attr", "attrs"),
    ("bs4", "beautifulsoup4"),
    ("community", "python-louvain"),
    ("cv2", "opencv-python-headless"),
    ("dateutil", "python-dateutil"),
    ("docx", "python-docx"),
    ("dotenv", "python-dotenv"),
    ("fitz", "pymupdf"),
    ("gi", "pygobject"),
    ("google.auth", "google-auth"),
    ("google.cloud.bigquery", "google-cloud-bigquery"),
    ("google.cloud.storage", "google-cloud-storage"),
    ("google.protobuf", "protobuf"),
    ("jwt", "pyjwt"),
    ("magic", "python-magic"),
    ("mpl_toolkits", "matplotlib"),
    ("osgeo", "gdal"),
    ("pptx", "python-pptx"),
    ("ruamel", "ruamel.yaml"),
    ("serial", "pyserial"),
    ("skimage", "scikit-image"),
    ("sklearn", "scikit-learn"),
    ("slugify", "python-slugify"),
    ("upath", "universal-pathlib"),
    ("yaml", "pyyaml"),
    ("zmq", "pyzmq"),
];

/// The PyPI packages that give a module: from the table, else the name of its top package,
/// as PyPI names most packages.
pub fn pypi_packages(module: &str) -> Vec<String> {
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
    vec![module.split('.').next().unwrap_or(module).to_owned()]
}

#[cfg(test)]
mod tests {
    use super::pypi_packages;

    #[test]
    fn resolves_package_names() {
        assert_eq!(pypi_packages("PIL.Image"), ["pillow"]);
        assert_eq!(pypi_packages("Bio.SeqIO"), ["biopython"]);
        assert_eq!(pypi_packages("cv2"), ["opencv-python-headless"]);
        assert_eq!(
            pypi_packages("google.cloud.storage"),
            ["google-cloud-storage"]
        );
        assert_eq!(pypi_packages("google.protobuf.json_format"), ["protobuf"]);
        assert_eq!(pypi_packages("google.auth"), ["google-auth"]);
        assert_eq!(pypi_packages("pyfaidx"), ["pyfaidx"]);
        assert_eq!(pypi_packages("earth_osm.eo"), ["earth_osm"]);
    }
}
