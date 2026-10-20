//! The helper that a node runs around a task without shared folders: it downloads the inputs of
//! a plan into the folder of the attempt and uploads the captured outputs and control records.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The copy of the plan that stage-in keeps in the folder for stage-out.
pub const PLAN: &str = "plan.json";
/// The folder of numbered output files that the capture wrapper fills.
pub const TRANSFER: &str = "transfer";
/// The largest file that one upload sends: S3 takes at most 5 GiB in one request.
pub const MOST_BYTES: u64 = 5 << 30;
/// The environment variable that holds the storage providers of the staging root, as the
/// `[storage.<name>]` tables of a config file, for a plan of object addresses.
pub const STORAGE: &str = "IUNCTUS_STAGING_STORAGE";

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

/// Runs `__stage in <folder> <plan> [<control>]`, `__stage out <folder> [<control>]` or
/// `__stage install <file>`. The plan stays in `<control>`, a folder that the task cannot
/// reach, else in `<folder>`.
pub fn main(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [command, folder, plan, control @ ..] if command == "in" && control.len() < 2 => {
            let folder = Path::new(folder);
            let keep = control.first().map_or(folder, Path::new);
            let text = read_plan(keep, plan)?;
            let parsed: Plan =
                serde_json::from_str(&text).map_err(|error| format!("invalid plan: {error}"))?;
            stage_in(folder, &parsed)?;
            std::fs::write(keep.join(PLAN), text).map_err(|error| error.to_string())
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
        _ => Err(
            "use `__stage in <folder> <plan> [<control>]`, `__stage out <folder> [<control>]` \
             or `__stage install <file>`"
                .into(),
        ),
    }
}

/// Makes the directories of a plan and downloads its files into `folder`, checking each digest.
/// Paths must stay in the folder, and no symbolic link on the way is followed.
pub fn stage_in(folder: &Path, plan: &Plan) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let client = client()?;
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
    let client = client()?;
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
        send(&client()?, &request, None)?
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
    if let Some(path) = request.url.strip_prefix("file://") {
        let path = PathBuf::from(storage::address::decode(path));
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
    // A request without a body waits at most `IDLE` for its answer and for each read after.
    built = match body {
        Some((file, size)) => built.body(reqwest::blocking::Body::sized(file, size)),
        None => built.timeout(IDLE),
    };
    let response = built.send().map_err(|error| without_url(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    Ok(Box::new(response))
}

/// The HTTP client of the helper. It trusts the certificates of the system and the roots of
/// Mozilla, as the image of a task may have no certificates of its own. An upload has no read
/// to time, so the kernel drops a connection whose data stays unacknowledged for `IDLE`.
fn client() -> Result<reqwest::blocking::Client, String> {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|root| reqwest::Certificate::from_der(root.as_ref()).ok());
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(None)
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
