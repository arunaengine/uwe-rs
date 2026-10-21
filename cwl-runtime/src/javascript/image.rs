//! Builds the image of the JavaScript helper from the running iunctus binary when it is missing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use executor::{CancellationToken, ContainerEngine};
use sha2::{Digest, Sha256};

/// The recipe that `scripts/cwl-js-image.py` builds too.
const DOCKERFILE: &str = include_str!("../../../containers/cwl-js/Dockerfile");

/// The repository of the helper images; each tag is the digest of the binary inside.
const REPOSITORY: &str = "uwe-cwl-js";

/// Names a static Linux binary of iunctus to make the helper image from, instead of the
/// running binary, for example on macOS.
const BINARY_VARIABLE: &str = "UWE_CWL_JS_BINARY";

/// `uwe-cwl-js:<digest>` of the running binary, made with the engine when it lacks it, so
/// that a new iunctus never uses the helper of an old one.
pub(crate) fn own_image(engine: &str, cancel: &CancellationToken) -> Result<String, String> {
    remember(engine, cancel, || make_image(engine, cancel))
}

/// The image of an engine that `make` gives, made once per process. A failure is remembered
/// too, as making it again would fail the same way, unless the run was cancelled.
fn remember(
    engine: &str,
    cancel: &CancellationToken,
    make: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    static KNOWN: Mutex<BTreeMap<String, Result<String, String>>> = Mutex::new(BTreeMap::new());
    let mut known = KNOWN.lock().map_err(|error| error.to_string())?;
    if let Some(result) = known.get(engine) {
        return result.clone();
    }
    let result = make();
    if result.is_ok() || !cancel.is_cancelled() {
        known.insert(engine.to_owned(), result.clone());
    }
    result
}

fn make_image(engine: &str, cancel: &CancellationToken) -> Result<String, String> {
    let configured = std::env::var_os(BINARY_VARIABLE).map(PathBuf::from);
    let binary = match &configured {
        Some(binary) => binary.clone(),
        None => std::env::current_exe().map_err(|error| error.to_string())?,
    };
    let machine = linux_machine(&binary, configured.is_some())?;
    let digest = file_digest(&binary).map_err(|error| error.to_string())?;
    let tag = format!("{REPOSITORY}:{digest}");
    let context = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    let file = context.reopen().map_err(|error| error.to_string())?;
    let failed = |error: io::Error| format!("cannot pack the JavaScript helper: {error}");
    pack(&binary, &digest, file).map_err(failed)?;
    let (name, cancel) = (engine.to_owned(), cancel.clone());
    let image = tag.clone();
    // The engine client needs a runtime of its own, outside the caller's.
    std::thread::spawn(move || build(&name, context.path(), (&image, machine), &cancel))
        .join()
        .map_err(|_| "the helper image build panicked".to_owned())??;
    Ok(tag)
}

/// The ELF machine of a binary that can run in a Linux helper container: a 64-bit
/// little-endian ELF file, and static when it is not the running binary, whose loaded
/// libraries go into the image.
fn linux_machine(binary: &Path, configured: bool) -> Result<u16, String> {
    let shown = binary.display();
    let machine = elf_machine(binary).map_err(|error| format!("cannot read {shown}: {error}"))?;
    let Some(machine) = machine else {
        return Err(format!(
            "the JavaScript helper of CWL runs iunctus in a Linux container, but {shown} is not \
             a Linux binary. {}",
            linux_help()
        ));
    };
    let dynamic = interpreter(binary).map_err(|error| error.to_string())?;
    if configured && dynamic.is_some() {
        return Err(format!(
            "{BINARY_VARIABLE} must name a static Linux binary of iunctus, but {shown} needs \
             the libraries of {}",
            dynamic.unwrap_or_default()
        ));
    }
    Ok(machine)
}

/// How to give the helper a Linux binary or image.
fn linux_help() -> String {
    format!(
        "Set {BINARY_VARIABLE} to a static Linux binary of iunctus for the architecture of the \
         container engine (`just release-binary` makes one), or UWE_CWL_JS_IMAGE to a \
         helper image built with `scripts/cwl-js-image.py`."
    )
}

/// The ELF machine of the containers of an engine of this architecture, as Docker names it.
fn engine_machine(architecture: &str) -> Option<u16> {
    match architecture {
        "x86_64" | "amd64" => Some(62),
        "aarch64" | "arm64" => Some(183),
        _ => None,
    }
}

fn build(
    engine: &str,
    context: &Path,
    (tag, machine): (&str, u16),
    cancel: &CancellationToken,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let engine = ContainerEngine::named(Some(engine).filter(|name| !name.is_empty()))
            .await
            .map_err(|error| error.to_string())?;
        // A binary of another architecture cannot start in the containers of the engine.
        let info = engine.client().info().await.ok();
        let architecture = info.and_then(|info| info.architecture).unwrap_or_default();
        if engine_machine(&architecture).is_some_and(|expected| expected != machine) {
            return Err(format!(
                "the binary for the JavaScript helper of CWL is not for the {architecture} \
                 containers of the engine. {}",
                linux_help()
            ));
        }
        // Older helpers, tagged by the digest of their binary, go once the new one exists.
        let older = |reference: &str| {
            let digest = reference
                .strip_prefix(REPOSITORY)
                .and_then(|tag| tag.strip_prefix(':'));
            digest.is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
        };
        (engine.build_replacing(context, tag, older, cancel).await)
            .map_err(|error| format!("cannot build the JavaScript helper image {tag}: {error}"))
    })
}

/// Writes the build context: the recipe, the binary, the libraries it loaded with its
/// interpreter, and their digests, as `scripts/cwl-js-image.py` does.
fn pack(binary: &Path, digest: &str, file: File) -> io::Result<()> {
    let mut files = BTreeMap::from([("/usr/local/bin/uwe".to_owned(), binary.to_owned())]);
    let mut digests = BTreeMap::from([("/usr/local/bin/uwe".to_owned(), digest.to_owned())]);
    if let Some(interpreter) = interpreter(binary)? {
        files.insert(interpreter.clone(), std::fs::canonicalize(&interpreter)?);
        for library in libraries(binary)? {
            files.insert(library.to_string_lossy().into_owned(), library);
        }
    }
    let mut folders: Vec<String> = Vec::new();
    for (location, source) in &files {
        if !digests.contains_key(location) {
            digests.insert(location.clone(), file_digest(source)?);
        }
        let folder = location.rsplit_once('/').map_or("", |(folder, _)| folder);
        if location != "/usr/local/bin/uwe" && !folders.iter().any(|known| known == folder) {
            folders.push(folder.to_owned());
        }
    }
    let mut recipe = DOCKERFILE.to_owned();
    if !folders.is_empty() {
        recipe.push_str(&format!("ENV LD_LIBRARY_PATH={}\n", folders.join(":")));
    }
    let mut archive = tar::Builder::new(file);
    let mut add = |name: &str, data: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive.append_data(&mut header, name, data)
    };
    add("Dockerfile", recipe.as_bytes(), 0o644)?;
    let identities: String = (digests.iter())
        .map(|(location, digest)| format!("{digest}  {location}\n"))
        .collect();
    add(
        "rootfs/usr/share/uwe/cwl-js.sha256",
        identities.as_bytes(),
        0o644,
    )?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o1777);
    header.set_size(0);
    header.set_cksum();
    archive.append_data(&mut header, "rootfs/tmp", io::empty())?;
    for (location, source) in &files {
        let mut header = tar::Header::new_gnu();
        let mut opened = File::open(source)?;
        header.set_size(opened.metadata()?.len());
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, format!("rootfs{location}"), &mut opened)?;
    }
    archive.into_inner()?.sync_all()
}

/// The hex SHA-256 digest of a file.
fn file_digest(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The machine (`e_machine`) of a 64-bit little-endian ELF file; none for other files.
fn elf_machine(binary: &Path) -> io::Result<Option<u16>> {
    let mut header = [0; 20];
    let mut file = File::open(binary)?;
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    let elf = &header[..4] == b"\x7fELF" && header[4] == 2 && header[5] == 1;
    Ok(elf.then(|| u16::from_le_bytes([header[18], header[19]])))
}

/// The program interpreter (`PT_INTERP`) of a 64-bit little-endian ELF file; none for a
/// static binary.
fn interpreter(binary: &Path) -> io::Result<Option<String>> {
    let mut file = File::open(binary)?;
    let mut header = [0; 64];
    file.read_exact(&mut header)?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return Ok(None);
    }
    let number = |bytes: &[u8]| {
        bytes
            .iter()
            .rev()
            .fold(0u64, |sum, byte| sum << 8 | u64::from(*byte))
    };
    let (table, size, count) = (
        number(&header[32..40]),
        number(&header[54..56]),
        number(&header[58..60]),
    );
    for index in 0..count.min(256) {
        let mut entry = [0; 56];
        file.seek(SeekFrom::Start(table + index * size))?;
        file.read_exact(&mut entry)?;
        if number(&entry[..4]) == 3 {
            let mut path = vec![0; number(&entry[32..40]).min(4096) as usize];
            file.seek(SeekFrom::Start(number(&entry[8..16])))?;
            file.read_exact(&mut path)?;
            let path = String::from_utf8_lossy(&path)
                .trim_end_matches('\0')
                .to_owned();
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// The shared libraries this process loaded, from `/proc/self/maps`: the binary needs no
/// others to start, and no external program like `ldd` has to run.
fn libraries(binary: &Path) -> io::Result<Vec<PathBuf>> {
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let own = std::fs::canonicalize(binary)?;
    let mut found: Vec<PathBuf> = Vec::new();
    for line in maps.lines() {
        let Some(path) = line
            .split_whitespace()
            .nth(5)
            .filter(|path| path.starts_with('/'))
        else {
            continue;
        };
        let path = PathBuf::from(path);
        let shared = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(".so"));
        if shared && path != own && path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary that is no Linux binary, like one of macOS, gets a message on what to set.
    #[test]
    fn explains_foreign_binaries() {
        let folder = tempfile::tempdir().expect("a folder");
        let binary = folder.path().join("uwe");
        std::fs::write(&binary, b"\xcf\xfa\xed\xfe\x07\x00\x00\x01 a Mach-O file").expect("writes");
        let error = linux_machine(&binary, false).unwrap_err();
        assert!(error.contains("is not a Linux binary"), "{error}");
        assert!(error.contains(BINARY_VARIABLE), "{error}");
        assert!(error.contains("UWE_CWL_JS_IMAGE"), "{error}");
        let running = std::env::current_exe().expect("the test binary");
        let machine = linux_machine(&running, false).expect("a Linux binary");
        assert_eq!(engine_machine(std::env::consts::ARCH), Some(machine));
        if interpreter(&running).expect("the ELF header").is_some() {
            let error = linux_machine(&running, true).unwrap_err();
            assert!(error.contains("must name a static Linux binary"), "{error}");
        }
    }

    /// A failed build is not tried again for the next expression, unless it was cancelled.
    #[test]
    fn remembers_failed_builds() {
        let cancel = CancellationToken::new();
        let tries = std::cell::Cell::new(0);
        let make = || {
            tries.set(tries.get() + 1);
            Err("no engine".to_owned())
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(remember("test-cancelled", &cancelled, make).is_err());
        assert!(remember("test-cancelled", &cancelled, make).is_err());
        assert_eq!(tries.get(), 2);
        assert!(remember("test-failed", &cancel, make).is_err());
        assert!(remember("test-failed", &cancel, make).is_err());
        assert_eq!(tries.get(), 3);
    }

    #[test]
    fn packs_running_binary() {
        let binary = std::env::current_exe().expect("the test binary");
        let context = tempfile::NamedTempFile::new().expect("a context file");
        pack(&binary, "digest", context.reopen().expect("a file")).expect("packs");
        let mut archive = tar::Archive::new(File::open(context.path()).expect("the context"));
        let mut names = Vec::new();
        let mut recipe = String::new();
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("an entry");
            let name = entry.path().expect("a path").to_string_lossy().into_owned();
            if name == "Dockerfile" {
                entry.read_to_string(&mut recipe).expect("the recipe");
            }
            names.push(name);
        }
        for name in [
            "rootfs/usr/local/bin/uwe",
            "rootfs/tmp",
            "rootfs/usr/share/uwe/cwl-js.sha256",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}: {names:?}");
        }
        assert!(recipe.contains("CMD [\"__cwl-js-worker\"]"), "{recipe}");
        // A dynamic binary brings its interpreter and the libraries it loaded.
        if let Some(interpreter) = interpreter(&binary).expect("the ELF header") {
            assert!(names.contains(&format!("rootfs{interpreter}")), "{names:?}");
            assert!(
                names.iter().any(|name| name.contains("libc.so")),
                "{names:?}"
            );
            assert!(recipe.contains("ENV LD_LIBRARY_PATH="), "{recipe}");
        }
    }
}
