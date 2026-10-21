//! Immutable preparation identity and a deterministic context for container-only installation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tar::{Builder, Header};

use crate::{FORMAT, Lock, VirtualPackage};

/// Version of a completed image receipt.
pub const IMAGE_RECEIPT_FORMAT: u32 = 1;

/// Evidence inspected from an already-built image and its optional distribution objects.
/// `preparation`, `base` and `recipe_digest` must come from immutable image metadata recorded
/// by the builder. A requested image name or tag is not evidence of that provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltImage {
    pub preparation: String,
    pub base: String,
    pub recipe_digest: String,
    pub platform: String,
    /// The image's configuration digest, called its image ID by Docker-compatible engines.
    pub image_id: String,
    /// The selected platform manifest digest in an OCI registry, when the image was published.
    pub manifest_digest: Option<String>,
    /// The digest of exported transport archive bytes, when those bytes were stored.
    pub archive_digest: Option<String>,
    /// A registry reference pinned to `manifest_digest`, when the image was published.
    pub reference: Option<String>,
}

/// A strictly validated receipt binding an immutable built image to its exact preparation.
/// The image ID, OCI manifest digest and transport archive digest are distinct identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletedImage {
    format: u32,
    preparation: String,
    base: String,
    recipe_digest: String,
    platform: String,
    image_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
}

/// Preparation for a resolved micromamba base. The caller must obtain the base's platform
/// and virtual capabilities from that target image, rather than the driver host.
#[derive(Clone, Debug)]
pub struct ImagePreparation {
    lock: Lock,
    base: String,
    platform: String,
    recipe: String,
    digest: String,
}

impl ImagePreparation {
    /// Binds a lock to an immutable base, explicit OCI platform and authoritative target
    /// capabilities. Unpinned pip requirements and unknown capability compatibility are refused.
    pub fn new(
        lock: &Lock,
        resolved_base: &str,
        platform: &str,
        capabilities: &[VirtualPackage],
    ) -> io::Result<Self> {
        let (name, base_digest) = resolved_base
            .rsplit_once("@sha256:")
            .ok_or_else(|| invalid("the base image must be pinned to SHA-256"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/:._-".contains(&byte))
            || !valid_digest(base_digest)
        {
            return Err(invalid("invalid resolved base image reference"));
        }
        let expected = match lock.platform.as_str() {
            "linux-64" => "linux/amd64",
            "linux-aarch64" => "linux/arm64",
            "linux-ppc64le" => "linux/ppc64le",
            "linux-s390x" => "linux/s390x",
            "linux-32" => "linux/386",
            _ => return Err(invalid("unsupported Conda image platform")),
        };
        if platform != expected && !(expected == "linux/arm64" && platform == "linux/arm64/v8") {
            return Err(invalid("Conda lock and target OCI platforms differ"));
        }
        let mut capabilities = capabilities.to_vec();
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        if lock.virtual_packages.as_ref() != Some(&capabilities) {
            return Err(invalid(
                "lock capabilities must match authoritative target metadata",
            ));
        }
        if lock.format != FORMAT {
            return Err(invalid(
                "image preparation needs a supported lock without unpinned pip requirements",
            ));
        }
        let pip = lock.pip_packages()?;
        for package in &lock.packages {
            if !package.sha256.as_deref().is_some_and(valid_digest)
                || !package
                    .file
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-+!".contains(&byte))
                || !(package.file.ends_with(".conda") || package.file.ends_with(".tar.bz2"))
                || package.name.starts_with("__")
            {
                return Err(invalid(
                    "image preparation needs real package files with SHA-256 identities",
                ));
            }
        }
        let base = format!("{name}@sha256:{}", base_digest.to_ascii_lowercase());
        let mut recipe = format!(
            "# uwe Conda image recipe 1\nFROM --platform={platform} {base}\n\
             USER root\nCOPY packages /tmp/uwe-conda-packages\n\
             COPY explicit.txt /tmp/uwe-conda-explicit.txt\n\
             RUN micromamba create --yes --offline --prefix /opt/uwe --file /tmp/uwe-conda-explicit.txt \
             && micromamba clean --all --yes && rm -rf /tmp/uwe-conda-packages /tmp/uwe-conda-explicit.txt\n\
             ENV PATH=/opt/uwe/bin:$PATH\nENTRYPOINT []\n"
        );
        if !pip.is_empty() {
            recipe.push_str("COPY pip /tmp/uwe-pip-packages\nCOPY pip.txt /tmp/uwe-pip.txt\n\
                RUN /opt/uwe/bin/python -m pip --isolated install --no-index --no-deps --no-build-isolation \
                --require-hashes --disable-pip-version-check -r /tmp/uwe-pip.txt \
                && rm -rf /tmp/uwe-pip-packages /tmp/uwe-pip.txt\n");
        }
        let identity = serde_json::to_vec(&(lock.digest(), &base, platform, &recipe))
            .map_err(io::Error::other)?;
        let digest = format!("sha256:{}", hex(Sha256::digest(&identity).as_slice()));
        Ok(Self {
            lock: lock.clone(),
            base,
            platform: platform.into(),
            recipe,
            digest,
        })
    }

    pub fn lock(&self) -> &Lock {
        &self.lock
    }

    /// Adds a pinned micromamba installer without changing the target base or capabilities.
    pub fn with_installer(
        lock: &Lock,
        base: &str,
        platform: &str,
        capabilities: &[VirtualPackage],
    ) -> io::Result<Self> {
        let mut preparation = Self::new(lock, base, platform, capabilities)?;
        let digest = match platform {
            "linux/amd64" => "955819619f303e2aa1dc1ba89beefe7f326a378b3d0782a4a0d28b1cf11b68b6",
            "linux/arm64" | "linux/arm64/v8" => {
                "555746adba7e231c903aab4f59d51a4b11ad35bc1cab50e3d78024652508bd64"
            }
            "linux/ppc64le" => "7c3ba4c2ccab952c235b34798ff7e3cb6aea912a7876386b95fe993d1207a926",
            _ => {
                return Err(invalid(
                    "pinned micromamba installer is unavailable for this platform",
                ));
            }
        };
        preparation.recipe = format!(
            "FROM --platform={platform} docker.io/mambaorg/micromamba:2.3.2@sha256:{digest} AS uwe_installer\n{}",
            preparation.recipe.replacen("USER root\n", "USER root\nCOPY --from=uwe_installer /bin/micromamba /usr/local/bin/micromamba\n", 1)
                .replace("RUN micromamba", "RUN /usr/local/bin/micromamba")
                .replace("&& micromamba clean", "&& /usr/local/bin/micromamba clean")
        );
        let identity = serde_json::to_vec(&(
            lock.digest(),
            &preparation.base,
            platform,
            &preparation.recipe,
        ))
        .map_err(io::Error::other)?;
        preparation.digest = format!("sha256:{}", hex(Sha256::digest(&identity).as_slice()));
        Ok(preparation)
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn platform(&self) -> &str {
        &self.platform
    }
    pub fn recipe(&self) -> &str {
        &self.recipe
    }

    /// The digest of the exact recipe bytes, for provenance recorded by an image builder.
    pub fn recipe_digest(&self) -> String {
        format!(
            "sha256:{}",
            hex(Sha256::digest(self.recipe.as_bytes()).as_slice())
        )
    }

    /// Registers inspected evidence of an already-built image as a portable receipt.
    pub fn completed(&self, image: BuiltImage) -> io::Result<CompletedImage> {
        let completed = CompletedImage {
            format: IMAGE_RECEIPT_FORMAT,
            preparation: image.preparation,
            base: image.base,
            recipe_digest: image.recipe_digest,
            platform: image.platform,
            image_id: image.image_id,
            manifest_digest: image.manifest_digest,
            archive_digest: image.archive_digest,
            reference: image.reference,
        };
        completed.validate(self)?;
        Ok(completed)
    }

    /// Writes a deterministic tar from the verified SHA-256 cache of [`crate::fetch_packages`].
    /// Errors may leave partial output: stage it and submit it to the container API only on success.
    /// Checks bytes while copying; never installs packages or builds an image on the driver host.
    pub fn write_context(&self, package_cache: &Path, out: &mut dyn Write) -> io::Result<()> {
        let mut archive = Builder::new(out);
        append(&mut archive, "Dockerfile", self.recipe.as_bytes())?;
        let mut explicit = format!("# platform: {}\n@EXPLICIT\n", self.lock.platform);
        for (index, package) in self.lock.packages.iter().enumerate() {
            explicit.push_str(&format!(
                "file:///tmp/uwe-conda-packages/{index}/{}\n",
                package.file
            ));
        }
        append(&mut archive, "explicit.txt", explicit.as_bytes())?;
        let pip = self.lock.pip_packages()?;
        if !pip.is_empty() {
            let mut requirements = String::new();
            for (index, package) in pip.iter().enumerate() {
                requirements.push_str(&format!(
                    "file:///tmp/uwe-pip-packages/{index}/{} --hash=sha256:{}\n",
                    package.file, package.sha256
                ));
            }
            append(&mut archive, "pip.txt", requirements.as_bytes())?;
        }
        if self.lock.packages.is_empty() {
            let mut header = header(0);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            archive.append_data(&mut header, "packages", io::empty())?;
        }
        let files = self
            .lock
            .packages
            .iter()
            .enumerate()
            .map(|(index, package)| {
                (
                    format!("packages/{index}/{}", package.file),
                    package.sha256.as_deref(),
                )
            })
            .chain(pip.iter().enumerate().map(|(index, package)| {
                (
                    format!("pip/{index}/{}", package.file),
                    Some(package.sha256.as_str()),
                )
            }));
        for (name, expected) in files {
            let expected = expected.ok_or_else(|| invalid("missing package digest"))?;
            let file = File::open(package_cache.join(expected.to_ascii_lowercase()))?;
            let size = file.metadata()?.len();
            let mut reader = Hashed {
                file,
                digest: Sha256::new(),
                size: 0,
            };
            archive.append_data(&mut header(size), name, &mut reader)?;
            if reader.size != size
                || !hex(reader.digest.finalize().as_slice()).eq_ignore_ascii_case(expected)
            {
                return Err(invalid(
                    "package cache bytes differ from lock SHA-256 identity",
                ));
            }
        }
        archive.finish()
    }
}

impl CompletedImage {
    /// Checks the receipt's format, provenance binding and distinct immutable identities.
    pub fn validate(&self, preparation: &ImagePreparation) -> io::Result<()> {
        if self.format != IMAGE_RECEIPT_FORMAT {
            return Err(invalid("unsupported completed image receipt format"));
        }
        if self.preparation != preparation.digest
            || self.base != preparation.base
            || self.recipe_digest != preparation.recipe_digest()
            || self.platform != preparation.platform
        {
            return Err(invalid(
                "completed image provenance differs from its preparation",
            ));
        }
        if !valid_sha256(&self.preparation)
            || !valid_sha256(&self.recipe_digest)
            || !valid_sha256(&self.image_id)
            || self
                .manifest_digest
                .as_deref()
                .is_some_and(|digest| !valid_sha256(digest))
            || self
                .archive_digest
                .as_deref()
                .is_some_and(|digest| !valid_sha256(digest))
        {
            return Err(invalid(
                "completed image receipt has an invalid SHA-256 identity",
            ));
        }
        if let Some(reference) = &self.reference {
            let Some((name, digest)) = reference.rsplit_once('@') else {
                return Err(invalid("completed image reference is not pinned"));
            };
            let valid_name = !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/:._-".contains(&byte));
            if !valid_name
                || !valid_sha256(digest)
                || self.manifest_digest.as_deref() != Some(digest)
            {
                return Err(invalid(
                    "completed image reference differs from its OCI manifest digest",
                ));
            }
        }
        Ok(())
    }

    pub fn format(&self) -> u32 {
        self.format
    }
    pub fn preparation(&self) -> &str {
        &self.preparation
    }
    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn recipe_digest(&self) -> &str {
        &self.recipe_digest
    }
    pub fn platform(&self) -> &str {
        &self.platform
    }
    pub fn image_id(&self) -> &str {
        &self.image_id
    }
    pub fn manifest_digest(&self) -> Option<&str> {
        self.manifest_digest.as_deref()
    }
    pub fn archive_digest(&self) -> Option<&str> {
        self.archive_digest.as_deref()
    }
    pub fn reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_sha256(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| valid_digest(hex) && hex.bytes().all(|byte| !byte.is_ascii_uppercase()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn header(size: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header
}

fn append<W: Write>(archive: &mut Builder<W>, path: &str, bytes: &[u8]) -> io::Result<()> {
    archive.append_data(&mut header(bytes.len() as u64), path, bytes)
}

struct Hashed {
    file: File,
    digest: Sha256,
    size: u64,
}

impl Read for Hashed {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(bytes)?;
        self.digest.update(&bytes[..count]);
        self.size += count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_installer_recipe() {
        let lock = Lock {
            format: FORMAT,
            platform: "linux-64".into(),
            channels: vec![],
            specs: vec![],
            pip: vec![],
            pypi: None,
            packages: vec![],
            virtual_packages: Some(vec![]),
        };
        for name in ["debian", "mambaorg/micromamba"] {
            let base = format!("{name}@sha256:{}", "a".repeat(64));
            let original = ImagePreparation::new(&lock, &base, "linux/amd64", &[]).unwrap();
            let prepared =
                ImagePreparation::with_installer(&lock, &base, "linux/amd64", &[]).unwrap();
            assert_eq!(prepared.base(), original.base());
            assert_ne!(prepared.digest(), original.digest());
            assert_ne!(prepared.recipe_digest(), original.recipe_digest());
            assert!(prepared.recipe().contains("micromamba:2.3.2@sha256:955819619f303e2aa1dc1ba89beefe7f326a378b3d0782a4a0d28b1cf11b68b6"));
            assert!(
                prepared.recipe().contains(
                    "COPY --from=uwe_installer /bin/micromamba /usr/local/bin/micromamba"
                )
            );
            assert!(
                prepared
                    .recipe()
                    .contains(&format!("FROM --platform=linux/amd64 {base}"))
            );
            assert_eq!(
                original.digest(),
                ImagePreparation::new(&lock, &base, "linux/amd64", &[])
                    .unwrap()
                    .digest()
            );
        }
    }
}
