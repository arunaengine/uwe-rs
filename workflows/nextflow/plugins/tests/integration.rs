//! Runs the integration test modules of this crate in one test binary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

#[path = "catalogue.rs"]
mod catalogue;
#[path = "crg_lims.rs"]
mod crg_lims;
#[path = "nf_agent.rs"]
mod nf_agent;
#[path = "nf_anndata.rs"]
mod nf_anndata;
#[path = "nf_arts.rs"]
mod nf_arts;
#[path = "nf_bactopia.rs"]
mod nf_bactopia;
#[path = "nf_bids.rs"]
mod nf_bids;
#[path = "nf_biffix.rs"]
mod nf_biffix;
#[path = "nf_bigbrother.rs"]
mod nf_bigbrother;
#[path = "nf_blocks.rs"]
mod nf_blocks;
#[path = "nf_boost.rs"]
mod nf_boost;
#[path = "nf_cloudcache.rs"]
mod nf_cloudcache;
#[path = "nf_cmgg.rs"]
mod nf_cmgg;
#[path = "nf_co2footprint.rs"]
mod nf_co2footprint;
#[path = "nf_core_utils/main.rs"]
mod nf_core_utils;
#[path = "nf_crukci_support.rs"]
mod nf_crukci_support;
#[path = "nf_csvext.rs"]
mod nf_csvext;
#[path = "nf_databricks.rs"]
mod nf_databricks;
#[path = "nf_datatrail.rs"]
mod nf_datatrail;
#[path = "nf_dotenv.rs"]
mod nf_dotenv;
#[path = "nf_drs.rs"]
mod nf_drs;
#[path = "nf_emoji.rs"]
mod nf_emoji;
#[path = "nf_fairscape.rs"]
mod nf_fairscape;
#[path = "nf_fgbio.rs"]
mod nf_fgbio;
#[path = "nf_fl86.rs"]
mod nf_fl86;
#[path = "nf_flowviewer.rs"]
mod nf_flowviewer;
#[path = "nf_gadi.rs"]
mod nf_gadi;
#[path = "nf_gc.rs"]
mod nf_gc;
#[path = "nf_gpt.rs"]
mod nf_gpt;
#[path = "nf_iridanext.rs"]
mod nf_iridanext;
#[path = "nf_irods.rs"]
mod nf_irods;
#[path = "nf_jev.rs"]
mod nf_jev;
#[path = "nf_labrobotics.rs"]
mod nf_labrobotics;
#[path = "nf_lakefs.rs"]
mod nf_lakefs;
#[path = "nf_lamin.rs"]
mod nf_lamin;
#[path = "nf_llm_debugger.rs"]
mod nf_llm_debugger;
#[path = "nf_logsaver.rs"]
mod nf_logsaver;
#[path = "nf_metalog.rs"]
mod nf_metalog;
#[path = "nf_nextpie.rs"]
mod nf_nextpie;
#[path = "nf_parquet.rs"]
mod nf_parquet;
#[path = "nf_ped.rs"]
mod nf_ped;
#[path = "nf_prometheus.rs"]
mod nf_prometheus;
#[path = "nf_prov.rs"]
mod nf_prov;
#[path = "nf_python.rs"]
mod nf_python;
#[path = "nf_r_ipc.rs"]
mod nf_r_ipc;
#[path = "nf_report.rs"]
mod nf_report;
#[path = "nf_runinsights.rs"]
mod nf_runinsights;
#[path = "nf_schema.rs"]
mod nf_schema;
#[path = "nf_seqlab_progress.rs"]
mod nf_seqlab_progress;
#[path = "nf_slack.rs"]
mod nf_slack;
#[path = "nf_snapshot.rs"]
mod nf_snapshot;
#[path = "nf_sqldb.rs"]
mod nf_sqldb;
#[path = "nf_storagemetrics.rs"]
mod nf_storagemetrics;
#[path = "nf_synapse.rs"]
mod nf_synapse;
#[path = "nf_teams.rs"]
mod nf_teams;
#[path = "nf_tencentcloud.rs"]
mod nf_tencentcloud;
#[path = "nf_tower.rs"]
mod nf_tower;
#[path = "nf_trino.rs"]
mod nf_trino;
#[path = "nf_validation.rs"]
mod nf_validation;
#[path = "nf_versions.rs"]
mod nf_versions;
#[path = "nf_weblog.rs"]
mod nf_weblog;
