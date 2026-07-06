//! Public S3 and Google Cloud Storage buckets, which anyone may read over HTTPS without
//! credentials, as anonymous clients do. This is what `s3://` and `gs://` addresses use when no
//! provider of the config file serves them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use crate::error::Problem;
use crate::web::Web;
use crate::{Entry, Metadata, address};

pub(crate) struct PublicBuckets {
    web: Web,
}

impl PublicBuckets {
    pub fn new() -> Result<PublicBuckets, Problem> {
        Ok(PublicBuckets {
            web: Web::public()?,
        })
    }

    /// Whether a file exists, and its size. A key that is a prefix of other keys is a directory.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let url = public_url(target)?;
        if let Some(found) = self.web.stat(&url).await? {
            return Ok(Some(found));
        }
        let (scheme, bucket, key) = parts(target)?;
        let prefix = format!("{}/", key.trim_end_matches('/'));
        let listing = format!(
            "{}?list-type=2&max-keys=1&prefix={prefix}",
            bucket_url(scheme, bucket)
        );
        // A bucket that does not allow listing answers with an error, and then nothing is known.
        let (_, text) = self.web.answer(&listing).await?;
        Ok(text.contains("<Key>").then_some(Metadata {
            directory: true,
            size: 0,
        }))
    }

    /// The entries of a directory: each name and whether it is a directory, from the S3
    /// listing of the keys below it.
    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let (scheme, bucket, key) = parts(target)?;
        let prefix = match key.trim_end_matches('/') {
            "" => String::new(),
            key => format!("{key}/"),
        };
        let mut entries = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut listing = format!(
                "{}?list-type=2&delimiter=/&prefix={prefix}",
                bucket_url(scheme, bucket)
            );
            if let Some(token) = &token {
                listing.push_str(&format!("&continuation-token={token}"));
            }
            let text = self.web.text(&listing).await?;
            for (tag, directory) in [("Key", false), ("Prefix", true)] {
                for name in elements(&text, tag) {
                    let name = name.strip_prefix(&prefix).unwrap_or(&name);
                    let name = name.trim_end_matches('/');
                    if !name.is_empty() {
                        entries.push(Entry {
                            name: name.to_owned(),
                            directory,
                        });
                    }
                }
            }
            token = elements(&text, "NextContinuationToken").into_iter().next();
            if token.is_none() {
                return Ok(entries);
            }
        }
    }

    pub async fn download(&self, target: &str, file: &Path, offset: u64) -> Result<bool, Problem> {
        self.web.download(&public_url(target)?, file, offset).await
    }
}

fn public_url(target: &str) -> Result<String, Problem> {
    address::public_url(target).ok_or_else(|| Problem::fixed("it is not an address of a bucket"))
}

/// The scheme, the bucket and the key of an address.
fn parts(target: &str) -> Result<(&str, &str, &str), Problem> {
    let scheme = address::scheme(target).unwrap_or_default();
    let (bucket, key) =
        address::bucket_and_key(target).ok_or_else(|| Problem::fixed("it names no bucket"))?;
    Ok((scheme, bucket, key))
}

/// The public address of a bucket, which also lists its keys as S3 does.
fn bucket_url(scheme: &str, bucket: &str) -> String {
    match scheme {
        "gs" => format!("https://storage.googleapis.com/{bucket}"),
        _ => format!("https://{bucket}.s3.amazonaws.com"),
    }
}

/// The texts of the XML elements with a tag, with the five entities of XML replaced.
fn elements(text: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        let value = after[..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        found.push(value);
        rest = &after[end + close.len()..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_give_keys_and_prefixes() {
        let text = "<ListBucketResult><Contents><Key>a/b.csv</Key></Contents>\
                    <CommonPrefixes><Prefix>a/c&amp;d/</Prefix></CommonPrefixes></ListBucketResult>";
        assert_eq!(elements(text, "Key"), ["a/b.csv"]);
        assert_eq!(elements(text, "Prefix"), ["a/c&d/"]);
    }
}
