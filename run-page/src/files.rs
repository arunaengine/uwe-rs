//! Downloads: the whole standard output or error of a task, and the files of a task. The page
//! serves only files that the database lists for the task, never a path that a request names.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Component, Path as FilePath, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use task_model::Digest;

use crate::{Failure, Shared, query};

/// The whole text of a log: from the store of a real run when it is a local directory, else
/// the text in the database, which is the end of a long log.
pub(crate) async fn log(
    State(state): State<Shared>,
    Path((run, task, attempt, stream)): Path<(String, u64, u32, String)>,
) -> Result<Response, Failure> {
    state.authorize(&run)?;
    if stream != "stdout" && stream != "stderr" {
        return Err(Failure::not_found("a log is `stdout` or `stderr`"));
    }
    let (log, store) = query(&state, move |database| {
        let log = database.log(&run, task, attempt, &stream)?;
        let store = database.run(&run)?.and_then(|run| run.store);
        Ok((log, store))
    })
    .await?;
    let Some(log) = log else {
        return Ok(text(String::new()));
    };
    if !log.whole
        && let Some(location) = &log.location
    {
        if !location.contains("://") {
            return send(FilePath::new(location), None).await;
        }
        if log.text.is_empty() {
            return Err(Failure::not_found(
                "the full log is retained in the remote store",
            ));
        }
    }
    if !log.whole
        && let (Some(store), Ok(digest)) = (store, log.digest.parse::<Digest>())
    {
        let path = FilePath::new(&store).join(content_store::layout::content(&digest));
        if path.is_file() {
            return send(&path, None).await;
        }
    }
    Ok(text(log.text))
}

fn text(text: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}

#[derive(Deserialize)]
pub(crate) struct Inside {
    /// A file inside a directory output, relative to it.
    path: Option<String>,
}

/// A file inside a directory, for the listing of a directory output.
#[derive(Serialize)]
struct Entry {
    path: String,
    size: u64,
}

/// A file of a task, as the page lists it: the input or the output of the last attempt at a
/// place in the list. Its content, or for a directory the list of files inside, or one of them
/// with `?path=`.
pub(crate) async fn file(
    State(state): State<Shared>,
    Path((run, task, role, index)): Path<(String, u64, String, usize)>,
    Query(inside): Query<Inside>,
) -> Result<Response, Failure> {
    state.authorize(&run)?;
    let file = query(&state, move |database| {
        let Some(detail) = database.task(&run, task)? else {
            return Err(Failure::not_found(format!("no task {task} in run `{run}`")));
        };
        let files = match role.as_str() {
            "inputs" => detail.inputs,
            "outputs" => detail.outputs,
            _ => return Err(Failure::not_found("files are `inputs` or `outputs`")),
        };
        files
            .into_iter()
            .nth(index)
            .ok_or_else(|| Failure::not_found(format!("task {task} has no such file")))
    })
    .await?;
    let location = PathBuf::from(&file.location);
    if file.location.is_empty() || file.location.contains("://") {
        return Err(Failure::not_found(format!(
            "{} is not a local file",
            file.location
        )));
    }
    if !location.is_dir() {
        return send(&location, Some(&file.path)).await;
    }
    match inside.path {
        Some(relative) => {
            let relative = FilePath::new(&relative);
            let plain = relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
            let target = location.join(relative);
            // A link inside the folder must not lead out of it either.
            let inside = match (target.canonicalize(), location.canonicalize()) {
                (Ok(target), Ok(root)) => target.starts_with(root),
                _ => false,
            };
            if !plain || !inside {
                return Err(Failure::bad_request("the path must stay inside the folder"));
            }
            send(&target, relative.to_str()).await
        }
        None => Ok(Json(listing(&location)?).into_response()),
    }
}

/// The files below a directory, by their paths relative to it.
fn listing(root: &FilePath) -> Result<Vec<Entry>, Failure> {
    let mut entries = Vec::new();
    let mut pending = vec![(root.to_owned(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type()?.is_dir() {
                pending.push((entry.path(), path));
            } else {
                let size = entry.metadata().map(|metadata| metadata.len())?;
                entries.push(Entry { path, size });
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Streams a file; with a name, as a download under the last part of that name.
async fn send(path: &FilePath, name: Option<&str>) -> Result<Response, Failure> {
    let opened = tokio::fs::File::open(path).await.map_err(|error| {
        Failure::not_found(format!("{} cannot be read: {error}", path.display()))
    })?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(opened));
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    let content = match name {
        Some(_) => "application/octet-stream",
        None => "text/plain; charset=utf-8",
    };
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content),
    );
    if let Some(name) = name.and_then(|name| name.rsplit('/').find(|part| !part.is_empty())) {
        let quoted: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_graphic() && c != '"' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if let Ok(value) = format!("attachment; filename=\"{quoted}\"").parse() {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok(response)
}
