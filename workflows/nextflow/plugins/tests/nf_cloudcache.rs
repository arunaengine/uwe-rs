//! nf-cloudcache: the `cloudcache` scope and `NXF_CLOUDCACHE_PATH` name the place of the store.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

#[test]
fn path_names_store() {
    let config = loaded(
        "plugins { id 'nf-cloudcache' }\ncloudcache {\n    enabled = true\n    \
         path = 's3://cachetest/cache2'\n}\n",
    );
    let found = nextflow_plugins::cloud_cache(&config, Some("s3://other/cache"));
    assert_eq!(found, Ok(Some("s3://cachetest/cache2".to_owned())));
    let config = loaded("cloudcache.enabled = true\n");
    let found = nextflow_plugins::cloud_cache(&config, Some("s3://cachetest/variable"));
    assert_eq!(found, Ok(Some("s3://cachetest/variable".to_owned())));
    let config = loaded("cloudcache.enabled = true\nworkDir = 's3://cachetest/work'\n");
    let found = nextflow_plugins::cloud_cache(&config, None);
    assert_eq!(found, Ok(Some("s3://cachetest/work".to_owned())));
}

#[test]
fn needs_enabled_remote() {
    let config = loaded("cloudcache.path = 's3://cachetest/cache2'\n");
    assert_eq!(nextflow_plugins::cloud_cache(&config, None), Ok(None));
    let config = loaded("cloudcache.enabled = 'true'\n");
    let found = nextflow_plugins::cloud_cache(&config, Some("s3://cachetest/cache"));
    assert_eq!(found, Ok(None), "only the boolean true turns it on");
    let config = loaded("cloudcache {\n    enabled = true\n    path = '/data/cache'\n}\n");
    assert_eq!(
        nextflow_plugins::cloud_cache(&config, None),
        Err("Storage path not supported by Cloud-cache - offending value: '/data/cache'".into())
    );
    let config = loaded("cloudcache.enabled = true\nworkDir = 'work'\n");
    let refused = nextflow_plugins::cloud_cache(&config, None).expect_err("a local work dir");
    assert!(refused.contains("offending value: 'work'"), "{refused}");
}
