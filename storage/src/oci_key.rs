//! Finds the API key of an OCI user, in the settings or in `~/.oci/config`, and signs requests
//! with it as the draft-cavage HTTP signatures of OCI need.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::oci::Options;

const DEFAULT_REGION: &str = "us-phoenix-1";

/// The key of a user, which signs requests, and the region of its profile.
pub struct Signer {
    /// `<tenancy>/<user>/<fingerprint>`.
    key_id: String,
    key: RsaKeyPair,
    pem: String,
    region: String,
}

/// The user and key of a profile.
struct Identity {
    tenancy: String,
    user: String,
    fingerprint: String,
    pem: String,
    region: String,
}

impl Signer {
    /// The key of the options, else of the profile of the config file.
    pub fn find(options: &Options) -> Result<Signer, String> {
        let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let home = variable("HOME").map(PathBuf::from);
        let identity = identity(options, &variable, home.as_deref())?;
        Ok(Signer {
            key_id: format!(
                "{}/{}/{}",
                identity.tenancy, identity.user, identity.fingerprint
            ),
            key: key_pair(&identity.pem)?,
            pem: identity.pem,
            region: identity.region,
        })
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    /// The PEM text of the key, which messages must not show.
    pub fn secret(&self) -> &str {
        &self.pem
    }

    /// The `Authorization` header that signs these headers, in this order; the pseudo header
    /// `(request-target)` is the lower-case method and the path with its query.
    pub fn authorization(&self, signed: &[(&str, String)]) -> Result<String, String> {
        let text: Vec<String> = (signed.iter())
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        let names: Vec<&str> = signed.iter().map(|(name, _)| *name).collect();
        let mut signature = vec![0; self.key.public_modulus_len()];
        (self.key)
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                text.join("\n").as_bytes(),
                &mut signature,
            )
            .map_err(|_| "the OCI API key cannot sign the request".to_owned())?;
        Ok(format!(
            "Signature version=\"1\",keyId=\"{}\",algorithm=\"rsa-sha256\",headers=\"{}\",\
             signature=\"{}\"",
            self.key_id,
            names.join(" "),
            STANDARD.encode(signature)
        ))
    }
}

/// The user, key and region: all four values of the options, else the profile of the config
/// file, where other profiles take what they lack from `DEFAULT`.
fn identity(
    options: &Options,
    variable: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Result<Identity, String> {
    let region = (options.region.clone()).or_else(|| variable("OCI_DEFAULT_REGION"));
    if let (Some(tenancy), Some(user), Some(fingerprint), Some(pem)) = (
        &options.tenancy,
        &options.user,
        &options.fingerprint,
        &options.private_key,
    ) {
        return Ok(Identity {
            tenancy: tenancy.clone(),
            user: user.clone(),
            fingerprint: fingerprint.clone(),
            pem: pem.clone(),
            region: region.unwrap_or_else(|| DEFAULT_REGION.to_owned()),
        });
    }
    let profile = (options.profile.clone())
        .or_else(|| variable("OCI_PROFILE"))
        .or_else(|| variable("OCI_DEFAULT_PROFILE"))
        .unwrap_or_else(|| "DEFAULT".to_owned());
    let file = home.map(|home| home.join(".oci/config"));
    let text = (file.as_deref()).and_then(|file| std::fs::read_to_string(file).ok());
    let missing = || {
        format!(
            "no OCI credentials: set oci.tenantId, oci.userId, oci.fingerprint and \
             oci.privateKey, or write the profile {profile} with user, fingerprint, key_file and \
             tenancy in ~/.oci/config"
        )
    };
    let sections = sections(&text.ok_or_else(missing)?);
    let value = |key: &str| {
        (sections.get(&profile).and_then(|section| section.get(key)))
            .or_else(|| sections.get("DEFAULT").and_then(|section| section.get(key)))
            .cloned()
    };
    if value("pass_phrase").is_some() {
        return Err("uwe cannot read an OCI API key that has a pass phrase".to_owned());
    }
    let key_file = value("key_file").ok_or_else(missing)?;
    let key_file = match (key_file.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(&key_file),
    };
    let pem = std::fs::read_to_string(&key_file)
        .map_err(|error| format!("{}: {error}", key_file.display()))?;
    Ok(Identity {
        tenancy: value("tenancy").ok_or_else(missing)?,
        user: value("user").ok_or_else(missing)?,
        fingerprint: value("fingerprint").ok_or_else(missing)?,
        pem,
        region: (region.or_else(|| value("region"))).unwrap_or_else(|| DEFAULT_REGION.to_owned()),
    })
}

/// The `[profile]` sections of an OCI config file, with their `key = value` lines.
fn sections(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current = name.trim().to_owned();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let section = sections.entry(current.clone()).or_default();
            section.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    sections
}

/// An RSA key from PEM text, PKCS#8 (`PRIVATE KEY`) or PKCS#1 (`RSA PRIVATE KEY`).
fn key_pair(pem: &str) -> Result<RsaKeyPair, String> {
    if pem.contains("ENCRYPTED") {
        return Err("uwe cannot read an OCI API key that has a pass phrase".to_owned());
    }
    let body: String = (pem.lines())
        .map(str::trim)
        .filter(|line| !line.starts_with("-----") && !line.is_empty())
        .collect();
    let refused = || "the OCI API key is not an RSA private key in PEM form".to_owned();
    let der = STANDARD.decode(body).map_err(|_| refused())?;
    RsaKeyPair::from_pkcs8(&der)
        .or_else(|_| RsaKeyPair::from_der(&der))
        .map_err(|_| refused())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_inherit_default() {
        let home = tempfile::tempdir().expect("a folder");
        std::fs::create_dir(home.path().join(".oci")).expect("a folder");
        std::fs::write(home.path().join(".oci/key.pem"), "PEM").expect("a key");
        let config = "[DEFAULT]\nuser = ocid1.user.a\nfingerprint = aa:bb\n\
                      key_file = ~/.oci/key.pem\ntenancy = ocid1.tenancy.a\n\
                      region = eu-frankfurt-1\n\n[LAB]\nuser = ocid1.user.lab\n";
        std::fs::write(home.path().join(".oci/config"), config).expect("a config");
        let options = Options::default();
        let none = |_: &str| None;
        let found = identity(&options, &none, Some(home.path())).expect("an identity");
        assert_eq!(
            (&*found.user, &*found.region, &*found.pem),
            ("ocid1.user.a", "eu-frankfurt-1", "PEM")
        );
        let lab = |name: &str| (name == "OCI_PROFILE").then(|| "LAB".to_owned());
        let found = identity(&options, &lab, Some(home.path())).expect("an identity");
        assert_eq!(
            (&*found.user, &*found.tenancy),
            ("ocid1.user.lab", "ocid1.tenancy.a")
        );
        let missing = identity(&options, &none, None).err().unwrap_or_default();
        assert!(missing.starts_with("no OCI credentials"), "{missing}");
        assert!(key_pair("-----BEGIN PRIVATE KEY-----\nAAAA\n").is_err());
    }
}
