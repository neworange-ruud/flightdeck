//! `GET /download/{ticket}` — a worker handing out one file (v9,
//! `remote-control-pz6d`). The grant came first, over the WebSocket
//! ([`crate::web::download`]); this is where the bytes leave.
//!
//! In order, and every refusal before the first byte is sent:
//!
//! 1. **The method**: `GET` only. `HEAD` and the rest are `405` (a `HEAD`
//!    would otherwise spend the ticket for nothing).
//! 2. **The cookie**, checked by the same [`authenticate`] as `/ws` and
//!    `/upload`, refused in the same words ([`refusal_response`]).
//! 3. **The ticket**, redeemed for this cookie's credential: unknown, expired,
//!    used, or someone else's are all one `403 invalid_ticket`.
//! 4. **The file, again**: its granted path is canonicalised now and must
//!    still be itself, inside `$HOME`, and a regular file; it is opened
//!    without following a final link, and what was opened must be what the
//!    path names (same device and inode). Anything else — moved, removed,
//!    swapped for a link or a folder — is `409 file_changed`.
//!
//! Then the body: `Content-Length` is the size when opened, and exactly that
//! many bytes are sent (a file still growing is cut there), read in
//! [`CHUNK`]-byte pieces by a blocking reader a few chunks ahead of the
//! socket. A read that fails before the first byte is `500 read_failed`;
//! after the headers are out the body is aborted, which the client sees as a
//! short read.
//!
//! A `200` is `application/octet-stream` with an attachment
//! [`content_disposition`] naming the file, `Cache-Control: no-store` and
//! `X-Content-Type-Options: nosniff`. Every refusal is JSON `{ok: false,
//! reason, message}` with `Cache-Control: no-store`, like an upload's.

use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use axum::extract::Path as UrlPath;
use axum::http::Method;

use super::upload_route::Failure;
use super::*;
use crate::web::download::{content_disposition, DownloadGrant};

/// The route, as axum spells its parameter.
pub(super) const DOWNLOAD_ROUTE: &str = "/download/{ticket}";

/// How much one read takes from the file.
const CHUNK: usize = 64 * 1024;

/// How many chunks may wait for the socket: enough to keep it busy, few
/// enough that a slow client pushes back on the disk.
const READ_AHEAD: usize = 4;

/// One piece of the body, or why it stopped.
type Piece = std::io::Result<axum::body::Bytes>;

/// `GET /download/{ticket}`. See the module docs.
pub(super) async fn download_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(conn): ConnectInfo<PeerInfo>,
    method: Method,
    UrlPath(ticket): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET {
        let mut response = Failure::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "A download is fetched with GET.",
        )
        .response();
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET"));
        return response;
    }
    let address = rate_limit_address(conn.addr);
    let token = match authenticate(&shared, conn.addr, &headers) {
        Ok(token) => token,
        Err(failure) => {
            debuglog::log(&format!(
                "web DOWNLOAD refused address={address} reason={}",
                failure.as_str()
            ));
            let attempts = shared
                .credentials
                .unwrap_or_recover()
                .attempts_remaining(&address);
            return refusal_response(failure, attempts, shared.now_ms());
        }
    };
    let redeemed = shared
        .downloads
        .unwrap_or_recover()
        .redeem(&ticket, &token, shared.now_ms());
    let Some(grant) = redeemed else {
        debuglog::log(&format!(
            "web DOWNLOAD refused address={address} reason=invalid_ticket"
        ));
        return Failure::new(
            StatusCode::FORBIDDEN,
            "invalid_ticket",
            "This download is not (or no longer) allowed. Choose the file again.",
        )
        .response();
    };

    let name = grant.name.clone();
    let opened = {
        let grant = grant.clone();
        tokio::task::spawn_blocking(move || open_granted(&grant))
            .await
            .unwrap_or_else(|_| Err(reader_gone()))
    };
    let (file, length) = match opened {
        Ok(opened) => opened,
        Err(failure) => {
            debuglog::log(&format!(
                "web DOWNLOAD refused name={name} reason={}",
                failure.reason()
            ));
            return failure.response();
        }
    };

    let (tx, mut rx) = mpsc::channel::<Piece>(READ_AHEAD);
    {
        let name = name.clone();
        tokio::task::spawn_blocking(move || send_file(file, length, &name, &tx));
    }
    // The first piece decides between a 200 and a 500: nothing has been sent
    // yet, so a file that cannot be read is still an honest failure.
    let first = match rx.recv().await {
        Some(Ok(bytes)) => Some(bytes),
        Some(Err(e)) => {
            debuglog::log(&format!("web DOWNLOAD failed name={name} err={e}"));
            return read_failed(&e).response();
        }
        // Nothing to read: an empty file (a shorter one sends an error).
        None => None,
    };
    debuglog::log(&format!("web DOWNLOAD start name={name} bytes={length}"));
    let rest = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|piece| (piece, rx))
    });
    let body = match first {
        Some(bytes) => {
            Body::from_stream(futures_util::stream::once(async { Ok(bytes) }).chain(rest))
        }
        None => Body::empty(),
    };
    ok_response(body, &grant, length)
}

/// The `200` head for `grant`'s `length` bytes.
fn ok_response(body: Body, grant: &DownloadGrant, length: u64) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    // Always ASCII without controls (see `content_disposition`), so this
    // cannot fail; a name that somehow did is sent without the header rather
    // than not at all.
    if let Ok(value) = header::HeaderValue::from_str(&content_disposition(&grant.name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CONTENT_LENGTH, header::HeaderValue::from(length));
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

/// The file has moved, gone, or is no longer the regular file inside
/// `$HOME` that was granted.
fn file_changed(grant: &DownloadGrant) -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "file_changed",
        format!("{} changed on the worker. Choose it again.", grant.name),
    )
}

fn read_failed(e: &std::io::Error) -> Failure {
    Failure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "read_failed",
        format!("The worker could not read the file: {e}"),
    )
}

/// A blocking task that panicked or was cancelled.
fn reader_gone() -> Failure {
    Failure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "read_failed",
        "The worker's reader stopped unexpectedly.",
    )
}

/// Check the granted file again and open it: the open file and its size.
fn open_granted(grant: &DownloadGrant) -> Result<(std::fs::File, u64), Failure> {
    let changed = || file_changed(grant);
    let canonical = std::fs::canonicalize(&grant.path).map_err(|_| changed())?;
    if canonical != grant.path || !canonical.starts_with(&grant.home) {
        return Err(changed());
    }
    let named = std::fs::metadata(&canonical).map_err(|_| changed())?;
    if !named.is_file() {
        return Err(changed());
    }
    // No final link may be swapped in between the check and the open, and a
    // FIFO swapped in must not block the open.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&canonical)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR) => changed(),
            _ => read_failed(&e),
        })?;
    let opened = file.metadata().map_err(|e| read_failed(&e))?;
    // What was opened must be what the path named when it was checked.
    if !opened.is_file() || opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(changed());
    }
    Ok((file, opened.len()))
}

/// Send exactly `length` bytes of `file` down `tx`, a chunk at a time; an
/// error if the file ends sooner. Stops quietly when the receiver is gone
/// (the client hung up).
fn send_file(mut file: std::fs::File, length: u64, name: &str, tx: &mpsc::Sender<Piece>) {
    let mut left = length;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = usize::try_from(left).map_or(CHUNK, |left| left.min(CHUNK));
        let read = match file.read(&mut buf[..want]) {
            Ok(0) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("the file shrank to {} bytes", length - left),
            )),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => Err(e),
        };
        match read {
            Ok(n) => {
                let piece = axum::body::Bytes::copy_from_slice(&buf[..n]);
                if tx.blocking_send(Ok(piece)).is_err() {
                    debuglog::log(&format!(
                        "web DOWNLOAD interrupted name={name} sent={}",
                        length - left
                    ));
                    return;
                }
                left -= n as u64;
            }
            Err(e) => {
                debuglog::log(&format!(
                    "web DOWNLOAD cut short name={name} sent={} err={e}",
                    length - left
                ));
                let _ = tx.blocking_send(Err(e));
                return;
            }
        }
    }
    debuglog::log(&format!("web DOWNLOAD done name={name} bytes={length}"));
}
