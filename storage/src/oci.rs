//! The settings of OCI Object Storage clients.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// What an OCI client signs in with. Unset values come from the environment and the OCI
/// config file, `~/.oci/config`.
#[derive(Clone, Default)]
pub struct Options {
    /// The profile of the config file; else `OCI_PROFILE`, `OCI_DEFAULT_PROFILE`, `DEFAULT`.
    pub profile: Option<String>,
    pub region: Option<String>,
    /// With `user`, `fingerprint` and `private_key`, the config file is not read.
    pub tenancy: Option<String>,
    pub user: Option<String>,
    pub fingerprint: Option<String>,
    /// The API key as PEM text.
    pub private_key: Option<String>,
    /// The address of the service; `https://objectstorage.<region>.oraclecloud.com` without one.
    pub endpoint: Option<String>,
}
