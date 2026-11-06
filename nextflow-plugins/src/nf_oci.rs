//! nf-oci 0.0.1: `oci://` paths, read and written by the OCI Object Storage provider of storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["oci"],
    observer: Some(activate),
    ..Plugin::new("nf-oci", Kind::FileSystem, State::Implemented)
};

/// Hands the `oci` scope to the provider that serves `oci://` paths; the plugin hears no run
/// events.
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    storage::oci::use_scope(options(config));
    Ok(None)
}

fn options(config: &Config) -> storage::oci::Options {
    let text = |key| {
        (config.get(&["oci", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    storage::oci::Options {
        profile: text("profile"),
        region: text("region"),
        tenancy: text("tenantId"),
        user: text("userId"),
        fingerprint: text("fingerprint"),
        private_key: text("privateKey"),
        endpoint: None,
    }
}

#[cfg(test)]
mod tests {
    use nextflow_evaluator::interpreter::Host;

    use super::*;

    struct Quiet;

    impl Host for Quiet {
        fn print(&mut self, _: &str) {}
    }

    #[test]
    fn scope_names_options() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("nextflow.config");
        let text = "oci {\n    region = 'eu-frankfurt-1'\n    tenantId = 'ocid1.tenancy.a'\n    \
                    userId = 'ocid1.user.a'\n    fingerprint = 'aa:bb'\n    privateKey = 'PEM'\n}\n";
        std::fs::write(&file, text).expect("a config");
        let settings = nextflow_config::Options {
            profiles: None,
            params: Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        let config = nextflow_config::load(&[file], &settings, &mut Quiet).expect("loads");
        let found = options(&config);
        assert_eq!(
            (found.region, found.tenancy, found.user, found.fingerprint),
            (
                Some("eu-frankfurt-1".into()),
                Some("ocid1.tenancy.a".into()),
                Some("ocid1.user.a".into()),
                Some("aa:bb".into())
            )
        );
        assert_eq!(
            (found.private_key, found.profile),
            (Some("PEM".into()), None)
        );
    }
}
