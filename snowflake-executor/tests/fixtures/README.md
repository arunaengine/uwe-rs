<!-- Where the RSA key of the Snowflake key-pair tests comes from, and its fingerprint. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Snowflake fixtures

`snowflake_key.pem` is a 2048-bit RSA key in PKCS #8 form, made on 2026-11-06 for these tests
only with `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048` (OpenSSL 3.6.5). It
belongs to no Snowflake user.

Its public key fingerprint, as Snowflake documents it (base64 of the SHA-256 of the DER public
key), came from `openssl pkey -pubout -outform DER | openssl dgst -sha256 -binary | base64`:
`MLpAQwQyp9aYRu5FP5tJOGkCon9oME+QwsBqsNc1vwk=`.
