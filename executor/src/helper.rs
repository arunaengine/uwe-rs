//! The helper that a node runs around a task without shared folders: it downloads the inputs of
//! a plan into the folder of the attempt and uploads the captured outputs and control records.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub use crate::manifest::Discovery;

/// The copy of the plan that stage-in keeps in the folder for stage-out.
pub const PLAN: &str = "plan.json";
/// The folder of numbered output files that the capture wrapper fills.
pub const TRANSFER: &str = "transfer";
/// The largest file that one upload sends: S3 takes at most 5 GiB in one request.
pub const MOST_BYTES: u64 = 5 << 30;
/// The environment variable that holds the storage providers of the staging root, as the
/// `[storage.<name>]` tables of a config file, for a plan of object addresses.
pub const STORAGE: &str = "UWE_STAGING_STORAGE";

/// How long a transfer may get no data before it fails and is tried again. A large file has no
/// limit on its whole time.
const IDLE: Duration = match cfg!(test) {
    true => Duration::from_secs(2),
    false => Duration::from_secs(120),
};

/// What a node places before a task starts and sends back after it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The files to download, by their paths in the folder.
    pub inputs: Vec<Input>,
    /// The directories to make in the folder.
    pub directories: Vec<String>,
    /// The upload of each numbered output file, from `transfer/1` on.
    pub files: Vec<Request>,
    /// The upload of each control record, in the order they are sent.
    pub records: Vec<Record>,
    /// Whether every user may change what stage-in makes, for a task that runs as another user
    /// than the helper.
    #[serde(default)]
    pub open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<Discovery>,
}

/// One file to download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
    pub request: Request,
}

/// One control file to upload from the control folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub name: String,
    pub request: Request,
}

/// A presigned request, a `file://` URL for a staging root in a folder, or the address of an
/// object, like `s3://bucket/key`, which the helper reaches with the providers of [`STORAGE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// Runs stage-in, stage-out, discovery, or installation from `__stage` arguments.
/// Stage-in keeps its plan in the separate control folder when supplied.
pub fn main(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [command, folder, plan, control @ ..] if command == "in" && control.len() < 2 => {
            let folder = Path::new(folder);
            let keep = control.first().map_or(folder, Path::new);
            let text = read_plan(keep, plan)?;
            let parsed: Plan =
                serde_json::from_str(&text).map_err(|error| format!("invalid plan: {error}"))?;
            // Kept first, so that stage-out sends the log of a failed stage-in back.
            std::fs::create_dir_all(keep).map_err(|error| error.to_string())?;
            std::fs::write(keep.join(PLAN), text).map_err(|error| error.to_string())?;
            stage_in(folder, &parsed)
        }
        [command, folder, control @ ..] if command == "out" && control.len() < 2 => {
            let (folder, control) = (Path::new(folder), control.first().map(Path::new));
            let text = std::fs::read_to_string(control.unwrap_or(folder).join(PLAN))
                .map_err(|error| format!("cannot read the plan of stage-in: {error}"))?;
            let plan: Plan =
                serde_json::from_str(&text).map_err(|error| format!("invalid plan: {error}"))?;
            stage_out(folder, control, &plan)
        }
        [command, file] if command == "install" => install(Path::new(file)),
        [command, folder, config] if command == "discover" => {
            let discovery: Discovery = serde_json::from_str(config)
                .map_err(|error| format!("invalid discovery: {error}"))?;
            discover(Path::new(folder), &discovery, 100_000)
        }
        _ => Err(
            "use `__stage in <folder> <plan> [<control>]`, `__stage out <folder> [<control>]` \
             `__stage discover <folder> <config-json>` or `__stage install <file>`"
                .into(),
        ),
    }
}

/// Makes the directories of a plan and downloads its files into `folder`, checking each digest.
/// Paths must stay in the folder, and no symbolic link on the way is followed.
pub fn stage_in(folder: &Path, plan: &Plan) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let client = client(Some(IDLE))?;
    for directory in &plan.directories {
        let path = inside(folder, directory)?;
        make_folders(folder, &path, plan.open)?;
    }
    for input in &plan.inputs {
        let path = inside(folder, &input.path)?;
        make_folders(folder, path.parent().expect("a parent"), plan.open)?;
        retried(|| download(&client, input, &path, plan.open))
            .map_err(|error| format!("cannot download {}: {error}", input.path))?;
    }
    Ok(())
}

/// Uploads the numbered output files and then the control records that exist. A record in
/// `control`, like the log of the job, comes before one that the task wrote.
pub fn stage_out(folder: &Path, control: Option<&Path>, plan: &Plan) -> Result<(), String> {
    if let Some(discovery) = &plan.discovery {
        discover(folder, discovery, plan.files.len())?;
    }
    let client = client(None)?;
    let mut number = 1;
    while let Ok(metadata) =
        std::fs::symlink_metadata(folder.join(TRANSFER).join(number.to_string()))
    {
        let request = plan.files.get(number - 1).ok_or_else(|| {
            format!(
                "the task has more than {} output files, which the plan allows",
                plan.files.len()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!("the output file {number} is not a regular file"));
        }
        let path = folder.join(TRANSFER).join(number.to_string());
        retried(|| upload(&client, &path, request))
            .map_err(|error| format!("cannot upload the output file {number}: {error}"))?;
        number += 1;
    }
    let is_file = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|data| data.is_file());
    for record in &plan.records {
        let path = inside(&folder.join(crate::CONTROL), &record.name)?;
        let own = control.map(|control| control.join(&record.name));
        let path = own.filter(|own| is_file(own)).unwrap_or(path);
        if is_file(&path) {
            retried(|| upload(&client, &path, &record.request))
                .map_err(|error| format!("cannot upload the record {}: {error}", record.name))?;
        }
    }
    Ok(())
}

/// Appends JSON-selected outputs to the existing container-side capture.
pub fn discover(folder: &Path, discovery: &Discovery, limit: usize) -> Result<(), String> {
    let folder = folder.canonicalize().map_err(|error| error.to_string())?;
    let folder = folder.as_path();
    discovery.check(folder)?;
    let mut finished =
        crate::Finished::from_control(folder.to_owned()).map_err(|error| error.to_string())?;
    if finished.exit_code != Some(0) {
        return Ok(());
    }
    let original = finished.files.len();
    discovery.collect(folder, &mut finished.files)?;
    let transfer = folder.join(TRANSFER);
    std::fs::create_dir_all(&transfer).map_err(|error| error.to_string())?;
    let mut index =
        std::fs::read_to_string(folder.join(crate::CONTROL).join(crate::capture::INDEX))
            .map_err(|error| error.to_string())?;
    let mut paths = std::collections::BTreeSet::new();
    for line in index.lines() {
        let (number, path) = line.split_once(' ').ok_or("the capture index is invalid")?;
        let number: usize = number.parse().map_err(|_| "the capture index is invalid")?;
        crate::manifest::parse(&format!("file {path}")).map_err(|error| error.to_string())?;
        if number != paths.len() + 1 || !paths.insert(path.trim_start_matches("./").to_owned()) {
            return Err("the capture index is not a sequence of unique paths".into());
        }
        let source = transfer.join(number.to_string());
        crate::manifest::check_path(folder, &source)?;
        if !std::fs::symlink_metadata(source).is_ok_and(|data| data.is_file()) {
            return Err("a captured output file is missing or is not a regular file".into());
        }
    }
    if paths.len() > limit {
        return Err(format!(
            "the task has more than {limit} output files, which the plan allows"
        ));
    }
    let (mut empty, captured) = (Vec::new(), paths.len());
    for file in &finished.files[original..] {
        let source = if !file.path.starts_with('/')
            && let Some(work) = &discovery.work
        {
            Path::new(work).join(&file.path)
        } else {
            finished.local_path(&file.path)
        };
        empty.extend(flatten(
            &source, &file.path, &transfer, &mut paths, &mut index, limit, 0,
        )?);
    }
    for path in empty {
        if !finished.files.iter().any(|file| file.path == path) {
            finished.files.push(crate::manifest::Collected {
                path,
                directory: true,
            });
        }
    }
    let manifest: String = finished
        .files
        .iter()
        .map(|file| {
            format!(
                "{} {}\n",
                if file.directory { "directory" } else { "file" },
                file.path
            )
        })
        .collect();
    std::fs::write(
        folder.join(crate::CONTROL).join(crate::wrapper::MANIFEST),
        manifest,
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        folder.join(crate::CONTROL).join(crate::capture::INDEX),
        index,
    )
    .map_err(|error| error.to_string())?;
    // The copies keep the mode of their source, so they tell which outputs are programs.
    let list = folder
        .join(crate::CONTROL)
        .join(crate::capture::EXECUTABLES);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(list);
    let mut programs = opened.map_err(|error| error.to_string())?;
    for number in captured + 1..=paths.len() {
        let data = std::fs::metadata(transfer.join(number.to_string()));
        let mode = data
            .map_err(|error| error.to_string())?
            .permissions()
            .mode();
        if mode & 0o111 != 0 {
            writeln!(programs, "{number}").map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn flatten(
    source: &Path,
    path: &str,
    transfer: &Path,
    paths: &mut std::collections::BTreeSet<String>,
    index: &mut String,
    limit: usize,
    depth: usize,
) -> Result<Vec<String>, String> {
    let mut empty = Vec::new();
    if depth > 64 || path.contains(['\n', '\r', '\0']) {
        return Err("an output path is invalid or exceeds depth 64".into());
    }
    let data = std::fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    if source.is_dir() {
        if data.is_symlink() {
            return Err("cannot capture directory links".into());
        }
        let mut entries = std::fs::read_dir(source)
            .map_err(|error| error.to_string())?
            .take(100_001)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        if entries.len() > 100_000 {
            return Err("an output directory exceeds 100000 entries".into());
        }
        if entries.is_empty() {
            empty.push(path.trim_start_matches("./").to_owned());
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "an output path is not UTF-8")?;
            empty.extend(flatten(
                &entry.path(),
                &format!("{path}/{name}"),
                transfer,
                paths,
                index,
                limit,
                depth + 1,
            )?);
        }
    } else if source.is_file() {
        let path = path.trim_start_matches("./");
        if paths.insert(path.to_owned()) {
            if paths.len() > limit {
                return Err(format!(
                    "the task has more than {limit} output files, which the plan allows"
                ));
            }
            let number = paths.len();
            let target = transfer.join(number.to_string());
            crate::manifest::check_path(
                transfer.parent().ok_or("the transfer has no parent")?,
                &target,
            )?;
            std::fs::copy(source, target).map_err(|error| error.to_string())?;
            index.push_str(&format!("{number} {path}\n"));
        }
    } else {
        return Err("an output is not a regular file".into());
    }
    Ok(empty)
}

/// Copies this program to `file`, for a container that gets it from an init container.
fn install(file: &Path) -> Result<(), String> {
    let program = std::env::current_exe().map_err(|error| error.to_string())?;
    std::fs::copy(program, file).map_err(|error| error.to_string())?;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| error.to_string())
}

/// The text of a plan from a URL, an object address or a local file. A plan at an address
/// goes to `folder` first.
fn read_plan(folder: &Path, location: &str) -> Result<String, String> {
    if !location.contains("://") {
        return std::fs::read_to_string(location).map_err(|error| error.to_string());
    }
    if is_object(location) {
        let path = folder.join(PLAN);
        std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        retried(|| (objects()?.download(location, &path)).map_err(|error| error.to_string()))
            .map_err(|error| format!("cannot download the plan: {error}"))?;
        return std::fs::read_to_string(path).map_err(|error| error.to_string());
    }
    let request = Request {
        method: "GET".into(),
        url: location.into(),
        headers: Vec::new(),
    };
    let mut text = String::new();
    retried(|| {
        text.clear();
        send(&client(Some(IDLE))?, &request, None)?
            .read_to_string(&mut text)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .map_err(|error| format!("cannot download the plan: {error}"))?;
    Ok(text)
}

/// The path of `relative` in `folder`, which must be a plain relative path.
fn inside(folder: &Path, relative: &str) -> Result<PathBuf, String> {
    let plain = !relative.starts_with('/')
        && !relative.contains('\0')
        && (relative.split('/')).all(|part| !part.is_empty() && part != "." && part != "..");
    match plain {
        true => Ok(folder.join(relative)),
        false => Err(format!(
            "the path {relative:?} leaves the folder of the task"
        )),
    }
}

/// Makes the folders from `folder` down to `path`, refusing links and files on the way.
fn make_folders(folder: &Path, path: &Path, open: bool) -> Result<(), String> {
    let mut current = folder.to_owned();
    for part in path
        .strip_prefix(folder)
        .expect("a path in the folder")
        .components()
    {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("{} is not a folder", current.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|error| error.to_string())?;
                if open {
                    let mode = std::fs::Permissions::from_mode(0o777);
                    std::fs::set_permissions(&current, mode).map_err(|error| error.to_string())?;
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Downloads an input to a new file beside its path, checks it and moves it in place.
fn download(
    client: &reqwest::blocking::Client,
    input: &Input,
    path: &Path,
    open: bool,
) -> Result<(), String> {
    let partial = path.with_file_name(format!(
        ".{}.partial",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&partial);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| error.to_string())?;
    // An object goes to the file whole, and is read back for its digest.
    let object = is_object(&input.request.url);
    let mut source = match object {
        true => {
            (objects()?.download(&input.request.url, &partial))
                .map_err(|error| error.to_string())?;
            file = std::fs::File::open(&partial).map_err(|error| error.to_string())?;
            Box::new(file.try_clone().map_err(|error| error.to_string())?)
        }
        false => send(client, &input.request, None)?,
    };
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = vec![0; 1 << 16];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        if !object {
            file.write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
        }
        size += count as u64;
    }
    let digest = task_model::Digest::from_sha256(&hash.finalize());
    if size != input.size || digest.hex() != input.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "got {size} bytes with SHA-256 {}, not {} bytes with {}",
            digest.hex(),
            input.size,
            input.sha256
        ));
    }
    let mode = match (input.executable, open) {
        (true, false) => 0o755,
        (false, false) => 0o644,
        (true, true) => 0o777,
        (false, true) => 0o666,
    };
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("{} is placed twice", path.display()));
    }
    std::fs::rename(&partial, path).map_err(|error| error.to_string())
}

/// Uploads a file in one request, or an object in parts.
fn upload(
    client: &reqwest::blocking::Client,
    path: &Path,
    request: &Request,
) -> Result<(), String> {
    if is_object(&request.url) {
        return (objects()?.upload(path, &request.url)).map_err(|error| error.to_string());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MOST_BYTES {
        return Err(format!(
            "it has {size} bytes, but one upload takes at most {MOST_BYTES} bytes"
        ));
    }
    send(client, request, Some((file, size))).map(|_| ())
}

/// Sends a request, with a body for an upload, and gives the response body.
fn send(
    client: &reqwest::blocking::Client,
    request: &Request,
    body: Option<(std::fs::File, u64)>,
) -> Result<Box<dyn Read>, String> {
    if let Some(path) = local_file(&request.url) {
        return match body {
            Some((mut file, _)) => {
                let parent = path.parent().expect("a parent");
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                let mut target =
                    tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
                std::io::copy(&mut file, &mut target).map_err(|error| error.to_string())?;
                target.persist(&path).map_err(|error| error.to_string())?;
                Ok(Box::new(std::io::empty()))
            }
            None => Ok(Box::new(
                std::fs::File::open(&path).map_err(|error| error.to_string())?,
            )),
        };
    }
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut built = client.request(method, &request.url);
    for (name, value) in &request.headers {
        built = built.header(name, value);
    }
    if let Some((file, size)) = body {
        built = built.body(reqwest::blocking::Body::sized(file, size));
    }
    let response = built.send().map_err(|error| without_url(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    Ok(Box::new(response))
}

/// The path of a `file://` URL, for a staging root in a folder.
pub fn local_file(url: &str) -> Option<PathBuf> {
    let path = url.strip_prefix("file://")?;
    Some(PathBuf::from(storage::address::decode(path)))
}

/// The HTTP client of the helper, which trusts the system and Mozilla roots. With `idle`, the
/// answer and each read wait at most that long (a request timeout would limit the whole body).
/// An upload has no read to time, so the kernel drops a connection unacknowledged for `IDLE`.
fn client(idle: Option<Duration>) -> Result<reqwest::blocking::Client, String> {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|root| reqwest::Certificate::from_der(root.as_ref()).ok());
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(idle)
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_user_timeout(IDLE)
        .tls_certs_merge(roots)
        .build()
        .map_err(|error| without_url(&error))
}

/// Whether a request names an object of a storage provider instead of a URL.
fn is_object(url: &str) -> bool {
    !["http://", "https://", "file://"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

/// The storage providers of [`STORAGE`], read once. Their errors name where a value is wrong,
/// never a secret.
fn objects() -> Result<storage::Blocking, String> {
    static OBJECTS: OnceLock<Result<storage::Blocking, String>> = OnceLock::new();
    let objects = OBJECTS.get_or_init(|| {
        let text = std::env::var(STORAGE).map_err(|_| {
            format!("the plan names objects, but {STORAGE} holds no storage providers")
        })?;
        let config = config_file::ConfigFile::parse(&text, Path::new(STORAGE));
        let storage = config.and_then(|config| storage::Storage::from_config(&config));
        storage::trust_mozilla_roots();
        Ok(storage.map_err(|error| error.to_string())?.blocking())
    });
    objects.clone()
}

/// Runs a transfer up to three times, as networks fail now and then.
fn retried(mut transfer: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let mut tries = 0;
    loop {
        tries += 1;
        match transfer() {
            Err(_) if tries < 3 => std::thread::sleep(Duration::from_secs(tries)),
            result => return result,
        }
    }
}

/// The error of a request without its URL, whose signature grants access.
fn without_url(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "<presigned URL>");
    }
    text
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
