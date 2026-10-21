//! The static iunctus binary that iunctus uploads to the staging root, so that the jobs on the
//! nodes download it from there and stage their files with it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sha2::{Digest as _, Sha256};
use task_model::Digest;

/// A statically linked Linux binary, which runs on every Linux node of its machine.
#[derive(Clone, Debug)]
pub struct Binary {
    pub file: PathBuf,
    pub digest: Digest,
    pub machine: Machine,
    /// Whether the binary is in the staging root already, so that later attempts skip the check.
    pub uploaded: Arc<AtomicBool>,
}

/// The processor that a binary runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Machine {
    Amd64,
    Arm64,
}

impl Machine {
    /// The name of Kubernetes for the machine, as the label `kubernetes.io/arch` has it.
    pub fn kubernetes(self) -> &'static str {
        match self {
            Machine::Amd64 => "amd64",
            Machine::Arm64 => "arm64",
        }
    }

    /// The name that `uname -m` prints on Linux.
    pub fn uname(self) -> &'static str {
        match self {
            Machine::Amd64 => "x86_64",
            Machine::Arm64 => "aarch64",
        }
    }
}

impl Binary {
    /// Checks that `file` is a static Linux binary and reads its digest.
    pub fn open(file: &Path) -> Result<Binary, String> {
        let failed = |error: std::io::Error| format!("cannot read {}: {error}", file.display());
        let mut reader = std::fs::File::open(file).map_err(failed)?;
        let mut header = Vec::new();
        (&mut reader)
            .take(1 << 16)
            .read_to_end(&mut header)
            .map_err(failed)?;
        let machine = machine(&header).map_err(|reason| {
            format!(
                "{} cannot run on the nodes: {reason}; build a static binary with `just \
                 release-binary` and name it with helper_file",
                file.display()
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(&header);
        let mut buffer = vec![0; 1 << 20];
        loop {
            let read = reader.read(&mut buffer).map_err(failed)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Binary {
            file: file.to_owned(),
            digest: Digest::from_sha256(&hasher.finalize()),
            machine,
            uploaded: Arc::default(),
        })
    }
}

/// The machine of a 64-bit little-endian Linux ELF file from its start, if it needs no program
/// interpreter, which is what a dynamically linked binary has.
fn machine(header: &[u8]) -> Result<Machine, String> {
    if !header.starts_with(b"\x7fELF") {
        return Err("it is not a Linux binary".into());
    }
    if header.len() < 64 || header[4] != 2 || header[5] != 1 {
        return Err("it is not a 64-bit little-endian binary".into());
    }
    let half = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]) as usize;
    let machine = match half(18) {
        0x3e => Machine::Amd64,
        0xb7 => Machine::Arm64,
        _ => return Err("it is not for amd64 or arm64".into()),
    };
    let start = u64::from_le_bytes(header[32..40].try_into().expect("8 bytes")) as usize;
    let (size, count) = (half(54), half(56));
    for index in 0..count {
        let at = start.saturating_add(index.saturating_mul(size));
        let Some(kind) = header.get(at..at.saturating_add(4)) else {
            return Err("its program headers are cut off".into());
        };
        // `PT_INTERP`
        if kind == 3u32.to_le_bytes() {
            return Err("it is linked dynamically".into());
        }
    }
    Ok(machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ELF header for `machine` with one program header of the type `kind`.
    fn header(machine: u16, kind: u32) -> Vec<u8> {
        let mut header = vec![0; 64 + 56];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        header[32..40].copy_from_slice(&64u64.to_le_bytes());
        header[54..56].copy_from_slice(&56u16.to_le_bytes());
        header[56..58].copy_from_slice(&1u16.to_le_bytes());
        header[64..68].copy_from_slice(&kind.to_le_bytes());
        header
    }

    #[test]
    fn reads_static_machine() {
        assert_eq!(machine(&header(0x3e, 1)), Ok(Machine::Amd64));
        assert_eq!(machine(&header(0xb7, 1)), Ok(Machine::Arm64));
        let dynamic = machine(&header(0x3e, 3));
        assert!(dynamic.unwrap_err().contains("dynamically"));
        assert!(machine(&header(0x28, 1)).unwrap_err().contains("amd64"));
        let mut cut = header(0x3e, 1);
        cut.truncate(66);
        assert!(machine(&cut).unwrap_err().contains("cut off"));
        let macho = b"\xcf\xfa\xed\xfe".repeat(16);
        assert!(machine(&macho).unwrap_err().contains("not a Linux binary"));
    }

    #[test]
    fn opens_static_file() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("iunctus");
        let mut bytes = header(0xb7, 1);
        bytes.extend(vec![7; 3 << 20]);
        std::fs::write(&file, &bytes).unwrap();
        let binary = Binary::open(&file).unwrap();
        assert_eq!(binary.machine.kubernetes(), "arm64");
        assert_eq!(binary.machine.uname(), "aarch64");
        assert_eq!(binary.digest, Digest::sha256(&bytes));
        std::fs::write(&file, header(0x3e, 3)).unwrap();
        let error = Binary::open(&file).unwrap_err();
        assert!(error.contains("just release-binary"), "{error}");
    }
}
