//! Addresses of files in storage, like `s3://bucket/key`, `gs://bucket/key`,
//! `az://container/blob`, `https://host/path` or `ftp://host/path`: their parts, which
//! prefix serves them, and the public web address of a file in a public bucket.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The scheme of an address, like `s3` of `s3://bucket/key`; `None` for a local path.
pub fn scheme(address: &str) -> Option<&str> {
    let (scheme, _) = address.split_once("://")?;
    let valid = scheme
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte));
    (valid && !scheme.is_empty()).then_some(scheme)
}

/// Whether `prefix` serves `address`: the address starts with the prefix, which ends at the
/// end of a name, so that `s3://lab` serves `s3://lab/a` but not `s3://lab-data/a`.
pub fn serves(prefix: &str, address: &str) -> bool {
    let Some(rest) = address.strip_prefix(prefix) else {
        return false;
    };
    prefix.ends_with('/') || rest.is_empty() || rest.starts_with(['/', '?', '#'])
}

/// The bucket and the key of an address in a bucket: `s3://bucket/key`, or
/// `https://endpoint/bucket/key` for a service at its own address.
pub fn bucket_and_key(address: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = address.split_once("://")?;
    let rest = match scheme {
        "http" | "https" => rest.split_once('/')?.1,
        _ => rest,
    };
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    (!bucket.is_empty()).then_some((bucket, key))
}

/// The web address of a file that anyone may read: `http` and `https` addresses as they are,
/// and files of public S3 and Google Cloud Storage buckets at their public addresses. `None`
/// for other addresses.
pub fn public_url(address: &str) -> Option<String> {
    match scheme(address)? {
        "http" | "https" => Some(raw_github(address).unwrap_or_else(|| address.to_owned())),
        "s3" => {
            let (bucket, key) = bucket_and_key(address)?;
            Some(format!("https://{bucket}.s3.amazonaws.com/{key}"))
        }
        "gs" => {
            let (bucket, key) = bucket_and_key(address)?;
            Some(format!("https://storage.googleapis.com/{bucket}/{key}"))
        }
        _ => None,
    }
}

/// The address that GitHub redirects a file of a repository to: `github.com/o/r/raw/ref/path`
/// is served by `raw.githubusercontent.com/o/r/ref/path`. Going there directly saves the
/// redirect, and works where only the file host is reachable.
pub fn raw_github(address: &str) -> Option<String> {
    let rest = address.strip_prefix("https://github.com/")?;
    let mut parts = rest.splitn(4, '/');
    let (owner, repository) = (parts.next()?, parts.next()?);
    let (raw, file) = (parts.next()?, parts.next()?);
    (raw == "raw" && !file.is_empty())
        .then(|| format!("https://raw.githubusercontent.com/{owner}/{repository}/{file}"))
}

/// Decodes the `%XX` escapes of a part of an address; other text stays as it is.
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_have_parts() {
        assert_eq!(scheme("s3://bucket/key"), Some("s3"));
        assert_eq!(scheme("/data/file"), None);
        assert_eq!(scheme("a b://x"), None);
        assert_eq!(
            bucket_and_key("s3://bucket/a/b.txt"),
            Some(("bucket", "a/b.txt"))
        );
        assert_eq!(bucket_and_key("gs://bucket"), Some(("bucket", "")));
        assert_eq!(
            bucket_and_key("https://minio.lab:9000/bucket/a.txt"),
            Some(("bucket", "a.txt"))
        );
        assert_eq!(bucket_and_key("s3:///key"), None);
        assert_eq!(decode("a%20b%2Fc%zz"), "a b/c%zz");
    }

    #[test]
    fn prefixes_end_at_names() {
        assert!(serves("s3://lab", "s3://lab/a.txt"));
        assert!(serves("s3://lab", "s3://lab"));
        assert!(!serves("s3://lab", "s3://lab-data/a.txt"));
        assert!(serves("s3://lab-", "s3://lab-/a.txt"));
        assert!(serves("s3://", "s3://any/a.txt"));
        assert!(serves("https://host/private/", "https://host/private/a"));
        assert!(!serves("https://host/private/", "https://host/public/a"));
    }

    #[test]
    fn public_files_have_web_addresses() {
        assert_eq!(
            public_url("s3://ngi-igenomes/test-data/x.zip").as_deref(),
            Some("https://ngi-igenomes.s3.amazonaws.com/test-data/x.zip")
        );
        assert_eq!(
            public_url("gs://deepvariant/GIAB_v5q0/x.bed").as_deref(),
            Some("https://storage.googleapis.com/deepvariant/GIAB_v5q0/x.bed")
        );
        assert_eq!(
            public_url("https://github.com/nf-core/test-datasets/raw/methylseq/a.fastq.gz")
                .as_deref(),
            Some("https://raw.githubusercontent.com/nf-core/test-datasets/methylseq/a.fastq.gz")
        );
        assert_eq!(
            public_url("https://github.com/nf-core/modules/blob/master/README.md").as_deref(),
            Some("https://github.com/nf-core/modules/blob/master/README.md")
        );
        assert_eq!(public_url("az://container/blob"), None);
        assert_eq!(public_url("ftp://host/file"), None);
    }
}
