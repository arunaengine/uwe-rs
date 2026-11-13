//! Signs the key-pair JWTs of the Snowflake SQL API with the user's RSA key, with the claims
//! that Snowflake documents: issuer, subject, issue and expiry times.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
use rsa::sha2::{Digest, Sha256};
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::json;

/// How long a token is valid; Snowflake takes at most one hour.
const LIFETIME: u64 = 59 * 60;

/// The key of a user, with the names that Snowflake expects in its tokens.
#[derive(Clone)]
pub struct KeyPair {
    /// `<ACCOUNT>.<USER>`, the subject of the tokens.
    subject: String,
    /// `SHA256:` and the base64 of the SHA-256 of the public key.
    fingerprint: String,
    key: RsaPrivateKey,
}

impl std::fmt::Debug for KeyPair {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyPair")
            .field("subject", &self.subject)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl KeyPair {
    /// The key of `user` of `account` from its PEM text, PKCS #8 or PKCS #1, unencrypted.
    pub fn new(account: &str, user: &str, pem: &str) -> Result<KeyPair, String> {
        if pem.contains("ENCRYPTED PRIVATE KEY") {
            return Err(
                "uwe cannot read an encrypted private key; give it without a passphrase \
                        in a file only you can read"
                    .into(),
            );
        }
        let key = (RsaPrivateKey::from_pkcs8_pem(pem))
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
            .map_err(|_| "the private key is no RSA key in PEM form".to_owned())?;
        let public = (key.to_public_key().to_public_key_der())
            .map_err(|error| format!("cannot encode the public key: {error}"))?;
        let fingerprint = format!(
            "SHA256:{}",
            STANDARD.encode(Sha256::digest(public.as_bytes()))
        );
        Ok(KeyPair {
            subject: format!("{}.{}", account_part(account), user.to_ascii_uppercase()),
            fingerprint,
            key,
        })
    }

    /// A token issued at `now`, in seconds since 1970.
    pub fn token(&self, now: u64) -> Result<String, String> {
        let header = json!({"alg": "RS256", "typ": "JWT"});
        let claims = json!({
            "iss": format!("{}.{}", self.subject, self.fingerprint),
            "sub": self.subject,
            "iat": now,
            "exp": now + LIFETIME,
        });
        let encode = |value: &serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
        let signed = format!("{}.{}", encode(&header), encode(&claims));
        let signer = SigningKey::<Sha256>::new(self.key.clone());
        let signature = (signer.try_sign(signed.as_bytes()))
            .map_err(|error| format!("cannot sign the token: {error}"))?;
        Ok(format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

/// The account as tokens name it: upper case, without the region of an account locator, and
/// for a `.global` account without the part after its first `-`.
fn account_part(account: &str) -> String {
    let cut = match account.contains(".global") {
        true => account.find('-'),
        false => account.find('.'),
    };
    let part = cut
        .filter(|cut| *cut > 0)
        .map_or(account, |cut| &account[..cut]);
    part.to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;

    /// The fixed test key, and its fingerprint as `openssl` computed it.
    const PEM: &str = include_str!("../tests/fixtures/snowflake_key.pem");
    const FINGERPRINT: &str = "SHA256:MLpAQwQyp9aYRu5FP5tJOGkCon9oME+QwsBqsNc1vwk=";

    #[test]
    fn signs_documented_claims() {
        let pair = KeyPair::new("xy12345.eu-central-1.aws", "uwe_runner", PEM).expect("a key");
        let token = pair.token(1_700_000_000).expect("a token");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |part: &str| -> serde_json::Value {
            let bytes = URL_SAFE_NO_PAD.decode(part).expect("base64url");
            serde_json::from_slice(&bytes).expect("JSON")
        };
        assert_eq!(decode(parts[0]), json!({"alg": "RS256", "typ": "JWT"}));
        let claims = decode(parts[1]);
        assert_eq!(claims["sub"], "XY12345.UWE_RUNNER");
        assert_eq!(claims["iss"], format!("XY12345.UWE_RUNNER.{FINGERPRINT}"));
        assert_eq!(claims["iat"], 1_700_000_000);
        assert_eq!(claims["exp"], 1_700_000_000 + 3540);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("base64url");
        let signature = Signature::try_from(signature.as_slice()).expect("a signature");
        let verifier = VerifyingKey::<Sha256>::new(pair.key.to_public_key());
        let signed = format!("{}.{}", parts[0], parts[1]);
        assert!(verifier.verify(signed.as_bytes(), &signature).is_ok());
        assert!(!format!("{pair:?}").contains("BEGIN"));
    }

    #[test]
    fn names_accounts() {
        assert_eq!(account_part("myorg-lab"), "MYORG-LAB");
        assert_eq!(account_part("xy12345.us-east-2.aws"), "XY12345");
        assert_eq!(account_part("myorg-lab.global"), "MYORG");
        let encrypted = "-----BEGIN ENCRYPTED PRIVATE KEY-----\n";
        assert!(KeyPair::new("a", "b", encrypted).is_err());
        assert!(KeyPair::new("a", "b", "not a key").is_err());
    }
}
