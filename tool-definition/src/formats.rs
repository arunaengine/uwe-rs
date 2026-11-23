//! EDAM formats of common Galaxy datatypes and file name endings, for tools that name none.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

/// Galaxy datatypes with the EDAM format that Galaxy's own datatype registry gives them.
/// Compressed variants keep the format of their content, as Galaxy maps them.
const DATATYPES: &[(&str, &str)] = &[
    ("bam", "format_2572"),
    ("bcf", "format_3020"),
    ("bed", "format_3003"),
    ("bed6", "format_3003"),
    ("bed12", "format_3003"),
    ("bedgraph", "format_3583"),
    ("bigbed", "format_3004"),
    ("bigwig", "format_3006"),
    ("binary", "format_2333"),
    ("biom1", "format_3746"),
    ("clustal", "format_1982"),
    ("cram", "format_3462"),
    ("csv", "format_3752"),
    ("embl", "format_1927"),
    ("fasta", "format_1929"),
    ("fasta.gz", "format_1929"),
    ("fastq", "format_1930"),
    ("fastq.gz", "format_1930"),
    ("fastqillumina", "format_1931"),
    ("fastqsanger", "format_1932"),
    ("fastqsanger.gz", "format_1932"),
    ("fastqsanger.bz2", "format_1932"),
    ("fastqsolexa", "format_1933"),
    ("genbank", "format_1936"),
    ("gff", "format_2305"),
    ("gff3", "format_1975"),
    ("gtf", "format_2306"),
    ("gz", "format_3989"),
    ("h5", "format_3590"),
    ("html", "format_2331"),
    ("interval", "format_3475"),
    ("jpg", "format_3579"),
    ("json", "format_3464"),
    ("loom", "format_3913"),
    ("maf", "format_3008"),
    ("mol2", "format_3816"),
    ("mzml", "format_3244"),
    ("mzxml", "format_3654"),
    ("newick", "format_1910"),
    ("nexus", "format_1912"),
    ("nhx", "format_1910"),
    ("pdb", "format_1476"),
    ("pdf", "format_3508"),
    ("phylip", "format_1997"),
    ("png", "format_3603"),
    ("qname_sorted.bam", "format_2572"),
    ("sam", "format_2573"),
    ("sdf", "format_3814"),
    ("smi", "format_1196"),
    ("stockholm", "format_1961"),
    ("svg", "format_3604"),
    ("tabular", "format_3475"),
    ("tsv", "format_3475"),
    ("twobit", "format_3009"),
    ("txt", "format_2330"),
    ("unsorted.bam", "format_2572"),
    ("vcf", "format_3016"),
    ("vcf_bgzip", "format_3016"),
    ("wig", "format_3005"),
    ("xlsx", "format_3620"),
    ("xml", "format_2332"),
    ("yaml", "format_3750"),
    ("zip", "format_3987"),
];

/// File name endings with their EDAM format. A `.gz` or `.bz2` ending gives the format of the
/// ending before it, and the gzip format only when there is none.
const ENDINGS: &[(&str, &str)] = &[
    ("2bit", "format_3009"),
    ("aln", "format_1982"),
    ("bai", "format_3327"),
    ("bam", "format_2572"),
    ("bb", "format_3004"),
    ("bcf", "format_3020"),
    ("bed", "format_3003"),
    ("bedgraph", "format_3583"),
    ("bigbed", "format_3004"),
    ("bigwig", "format_3006"),
    ("bw", "format_3006"),
    ("cram", "format_3462"),
    ("csv", "format_3752"),
    ("embl", "format_1927"),
    ("fa", "format_1929"),
    ("faa", "format_1929"),
    ("fasta", "format_1929"),
    ("fastq", "format_1930"),
    ("fna", "format_1929"),
    ("fq", "format_1930"),
    ("gb", "format_1936"),
    ("gbk", "format_1936"),
    ("gff", "format_2305"),
    ("gff3", "format_1975"),
    ("gtf", "format_2306"),
    ("h5", "format_3590"),
    ("hdf5", "format_3590"),
    ("htm", "format_2331"),
    ("html", "format_2331"),
    ("jpeg", "format_3579"),
    ("jpg", "format_3579"),
    ("json", "format_3464"),
    ("log", "format_2330"),
    ("loom", "format_3913"),
    ("maf", "format_3008"),
    ("mzml", "format_3244"),
    ("nex", "format_1912"),
    ("nexus", "format_1912"),
    ("newick", "format_1910"),
    ("nwk", "format_1910"),
    ("pdb", "format_1476"),
    ("pdf", "format_3508"),
    ("phy", "format_1997"),
    ("png", "format_3603"),
    ("py", "format_3996"),
    ("r", "format_3999"),
    ("sam", "format_2573"),
    ("sdf", "format_3814"),
    ("sto", "format_1961"),
    ("svg", "format_3604"),
    ("tab", "format_3475"),
    ("tbi", "format_3616"),
    ("tsv", "format_3475"),
    ("txt", "format_2330"),
    ("vcf", "format_3016"),
    ("wig", "format_3005"),
    ("xlsx", "format_3620"),
    ("xml", "format_2332"),
    ("yaml", "format_3750"),
    ("yml", "format_3750"),
    ("zip", "format_3987"),
];

/// The EDAM format of a Galaxy datatype, like `format_1932` for `fastqsanger`.
pub fn datatype(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let found = DATATYPES.iter().find(|(known, _)| *known == name);
    found.map(|(_, term)| *term)
}

/// Every Galaxy datatype of the table with its EDAM format, as the converters take them.
pub fn datatypes() -> BTreeMap<String, String> {
    (DATATYPES.iter())
        .map(|(name, term)| ((*name).to_owned(), (*term).to_owned()))
        .collect()
}

/// The EDAM format of a file by the ending of its name or pattern, like `format_2572` for
/// `{sample}.bam` or `format_1930` for `reads.fq.gz`.
pub fn ending(name: &str) -> Option<&'static str> {
    let name = name
        .trim()
        .trim_end_matches(['"', '\''])
        .to_ascii_lowercase();
    let file = name.rsplit('/').next().unwrap_or(&name);
    let mut parts = file.rsplit('.');
    let last = parts.next()?;
    if !file.contains('.') {
        return None;
    }
    let known = |ending: &str| ENDINGS.iter().find(|(known, _)| *known == ending);
    match last {
        "gz" | "bz2" => match parts.next().and_then(known) {
            Some((_, term)) => Some(term),
            None if last == "gz" => Some("format_3989"),
            None => None,
        },
        ending => known(ending).map(|(_, term)| *term),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edam::hierarchy;

    #[test]
    fn known_edam_formats() {
        let edam = hierarchy();
        for (name, term) in DATATYPES.iter().chain(ENDINGS) {
            assert!(edam.contains(term), "{name}: {term} is not in EDAM");
            assert!(term.starts_with("format_"), "{name}: {term} is no format");
        }
    }

    #[test]
    fn compressed_endings() {
        assert_eq!(ending("reads.fq.gz"), Some("format_1930"));
        assert_eq!(ending("results/{sample}.bam"), Some("format_2572"));
        assert_eq!(ending("archive.gz"), Some("format_3989"));
        assert_eq!(ending("table.unknown.bz2"), None);
        assert_eq!(ending("Makefile"), None);
        assert_eq!(datatype("fastqsanger"), Some("format_1932"));
        assert_eq!(datatype("FASTA"), Some("format_1929"));
        assert_eq!(datatype("data"), None);
    }
}
