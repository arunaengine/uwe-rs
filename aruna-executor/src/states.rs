//! Stable Aruna wire names, including values added by newer servers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            Unknown(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value),
                })
            }
        }
    };
}

wire_enum!(JobState {
    Queued => "queued", Claimed => "claimed", Preparing => "preparing", Ready => "ready",
    Running => "running", Cancelling => "cancelling", Indeterminate => "indeterminate",
    Succeeded => "succeeded", Failed => "failed", Cancelled => "cancelled",
    AwaitingKey => "awaiting_key",
});

wire_enum!(JobKind {
    Probe => "probe", Execution => "execution", WriteRunCrate => "write_run_crate",
    TerminalCleanup => "terminal_cleanup", Staging => "staging", ImportRoCrate => "import_rocrate",
    ExportRoCrate => "export_rocrate", Harvest => "harvest", MintPersistentId => "mint_persistent_id",
    StoragePurge => "storage_purge", CopyObject => "copy_object", RegisterIdentifiers => "register_identifiers",
});

wire_enum!(LogicalState {
    Queued => "queued", Running => "running", Indeterminate => "indeterminate",
    Succeeded => "succeeded", Failed => "failed", Cancelled => "cancelled",
});

wire_enum!(ExecutionState {
    Accepted => "accepted", Preparing => "preparing", Running => "running",
    Succeeded => "succeeded", Failed => "failed", Cancelled => "cancelled", Error => "error",
});
