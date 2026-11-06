//! nf-lamin 0.9.0, in part: `lamin://` paths, read by the LaminDB provider of storage; the run
//! tracking of the plugin is not implemented yet.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["lamin"],
    observer: Some(activate),
    ..Plugin::new("nf-lamin", Kind::Observer, State::Partial)
};

/// Hands the `lamin` scope to the provider that serves `lamin://` paths.
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    tracing::warn!(
        "nf-lamin: uwe reads lamin:// paths, but does not record the run in LaminDB yet"
    );
    storage::lamin::use_scope(options(config));
    Ok(None)
}

fn options(config: &Config) -> storage::lamin::Options {
    let text = |path: &[&str]| {
        (config.get(path))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
            .filter(|text| !text.is_empty())
    };
    let hub = text(&["lamin", "api", "supabase_api_url"]);
    let environment = text(&["lamin", "env"]).or_else(|| std::env::var("LAMIN_ENV").ok());
    if hub.is_none() && environment.as_deref().is_some_and(|name| name != "prod") {
        tracing::warn!(
            "nf-lamin: uwe knows only the prod hub; set lamin.api.supabase_api_url for another"
        );
    }
    storage::lamin::Options {
        api_key: text(&["lamin", "api_key"]),
        hub,
        anon_key: text(&["lamin", "api", "supabase_anon_key"]),
        hub_credentials: text(&["lamin", "features", "manage_s3_credentials"])
            .map(|flag| flag == "true"),
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
        let text = "lamin {\n    instance = 'demo-org/demo-inst'\n    api_key = 'key'\n    \
                    api {\n        supabase_api_url = 'http://127.0.0.1:1'\n        \
                    supabase_anon_key = 'anon'\n    }\n    \
                    features { manage_s3_credentials = false }\n}\n";
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
            (
                found.api_key,
                found.hub,
                found.anon_key,
                found.hub_credentials
            ),
            (
                Some("key".into()),
                Some("http://127.0.0.1:1".into()),
                Some("anon".into()),
                Some(false)
            )
        );
    }
}
