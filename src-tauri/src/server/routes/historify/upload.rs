//! The file upload route (web `historify.py` `/upload`): a CSV or Parquet
//! file of candles for one symbol, streamed to a temporary file under the
//! size limit, then imported.

use super::{Ctx, MAX_UPLOAD_BYTES};
use crate::historify::import::FileKind;
use crate::server::api_v1::send;
use crate::server::envelope::{error, EarlyReply};
use crate::state::AppState;
use axum::{
    extract::{multipart::Field, Multipart, State},
    http::StatusCode,
    response::Response,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Removes the saved upload on every path.
struct UploadFile(PathBuf);

impl Drop for UploadFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn unreadable_upload() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "The upload could not be read. Try again.",
    )
}

/// POST /historify/api/upload (multipart: file, symbol, exchange, interval)
pub async fn upload(State(ctx): Ctx, mp: Multipart) -> Response {
    let form = match read_form(&ctx, mp).await {
        Ok(f) => f,
        Err(r) => return *r,
    };
    if !form.file_seen {
        return error(StatusCode::BAD_REQUEST, "No file provided");
    }
    if let Some(r) = form.bad_name {
        return r;
    }
    let Some((file, kind)) = form.saved else {
        return error(StatusCode::BAD_REQUEST, "No file provided");
    };
    let f = |k: &str| {
        form.fields
            .get(k)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let (symbol, exchange, interval) = (
        f("symbol").to_uppercase(),
        f("exchange").to_uppercase(),
        f("interval"),
    );
    if symbol.is_empty() || exchange.is_empty() || interval.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "Symbol, exchange, and interval are required",
        );
    }
    let r = ctx
        .historify
        .upload(&file.0, kind, &symbol, &exchange, &interval)
        .await;
    drop(file);
    send(r)
}

/// What the upload form carried.
#[derive(Default)]
struct UploadForm {
    /// The first `file` part, saved, and its kind.
    saved: Option<(UploadFile, FileKind)>,
    /// The text parts.
    fields: HashMap<String, String>,
    file_seen: bool,
    /// The answer for a `file` part with no name or another extension.
    bad_name: Option<Response>,
}

/// Read the form: the first `file` part streamed to a temporary file, the
/// other parts as text.
async fn read_form(ctx: &AppState, mut mp: Multipart) -> Result<UploadForm, EarlyReply> {
    let mut form = UploadForm::default();
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return Err(read_error(e.status()).into()),
        };
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" && !form.file_seen {
            form.file_seen = true;
            match file_kind(field.file_name().unwrap_or_default()) {
                Ok(kind) => form.saved = Some((save_part(ctx, field, kind).await?, kind)),
                Err(r) => form.bad_name = Some(*r),
            }
        } else if !name.is_empty() {
            let text = field.text().await.map_err(|_| unreadable_upload())?;
            form.fields.insert(name, text);
        }
    }
    Ok(form)
}

/// The kind of an uploaded file, from its name.
fn file_kind(filename: &str) -> Result<FileKind, EarlyReply> {
    let filename = filename.to_lowercase();
    if filename.is_empty() {
        Err(Box::new(error(StatusCode::BAD_REQUEST, "No file selected")))
    } else if filename.ends_with(".csv") {
        Ok(FileKind::Csv)
    } else if filename.ends_with(".parquet") {
        Ok(FileKind::Parquet)
    } else {
        Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "File must be CSV or Parquet",
        )))
    }
}

/// Stream one file part to a new file in the Historify work folder, under
/// the size limit. The guard removes the file on every path.
async fn save_part(
    ctx: &AppState,
    mut field: Field<'_>,
    kind: FileKind,
) -> Result<UploadFile, EarlyReply> {
    use tokio::io::AsyncWriteExt;
    let disk = || {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save the upload. Check free disk space.",
        )
    };
    let dir = match ctx.historify.work_dir() {
        Ok(d) => d.to_path_buf(),
        Err(e) => {
            tracing::error!("Could not create the Historify work folder: {}", e);
            return Err(disk().into());
        }
    };
    let ext = if kind == FileKind::Csv {
        "csv"
    } else {
        "parquet"
    };
    let path = dir.join(format!(
        "historify_upload_{}.{}",
        uuid::Uuid::new_v4().simple(),
        ext
    ));
    let guard = UploadFile(path.clone());
    let mut out = match tokio::fs::File::create(&path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Could not save an upload: {}", e);
            return Err(disk().into());
        }
    };
    let mut size = 0usize;
    while let Some(chunk) = field.chunk().await.map_err(|e| read_error(e.status()))? {
        size += chunk.len();
        if size > MAX_UPLOAD_BYTES {
            return Err(too_large().into());
        }
        if out.write_all(&chunk).await.is_err() {
            return Err(disk().into());
        }
    }
    if out.flush().await.is_err() {
        return Err(unreadable_upload().into());
    }
    drop(out);
    Ok(guard)
}

/// A part that could not be read: too large, or broken off.
fn read_error(status: StatusCode) -> Response {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        too_large()
    } else {
        unreadable_upload()
    }
}

fn too_large() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        format!(
            "File too large. Maximum size is {} MB",
            MAX_UPLOAD_BYTES / (1024 * 1024)
        ),
    )
}
