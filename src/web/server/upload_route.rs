//! `PUT /upload/{ticket}` — a worker receiving one file (v8,
//! `remote-control-usem`). The grant came first, over the WebSocket
//! ([`crate::web::upload`]); this is where the bytes land.
//!
//! In order, and every refusal before the first byte is written:
//!
//! 1. **The cookie**, checked by the same [`authenticate`] as `/ws` and
//!    `/auth/session`, refused in the same words ([`refusal_response`]).
//! 2. **The framing**: a `Content-Length` (`411` without one), no
//!    `Transfer-Encoding` (`400`): the size is part of the grant.
//! 3. **The ticket**, redeemed for this cookie's credential: unknown, expired,
//!    used, or someone else's are all one `403 invalid_ticket`; a length that
//!    is not the granted size is `400 size_mismatch` and keeps the grant.
//!
//! Then the grant is spent and the file is written to a hidden sibling temp
//! (`.<name>.fdupload-<random>`, created new, default mode) by a blocking
//! writer fed from the request body: at most [`IDLE_READ`] between two
//! chunks, and never a byte past the granted size. It is `fsync`ed, the folder
//! is canonicalised again and must still be the granted one inside `$HOME`
//! (a symlink swapped in meanwhile is refused), and the file is committed:
//! `replace` renames over the name; otherwise a hard link claims the name only
//! if it is still free (`409 exists` if not) and the temp goes. Whatever
//! ends the request early — a refusal, a timeout, the client hanging up —
//! drops [`Receipt`], which removes the temp and tells the host.
//!
//! Every answer is JSON with `Cache-Control: no-store`: `{ok: true, path,
//! bytes}`, or `{ok: false, reason, message}` — `reason` a stable code,
//! `message` a sentence for a person.

use std::io::Write;
use std::path::{Path, PathBuf};

use axum::extract::Path as UrlPath;

use super::*;
use crate::web::upload::{Redeem, UploadFinished, UploadGrant};

/// The route, as axum spells its parameter.
pub(super) const UPLOAD_ROUTE: &str = "/upload/{ticket}";

/// The longest the body may go quiet between two chunks.
const IDLE_READ: Duration = Duration::from_secs(60);

/// How many body chunks may wait for the writer: enough to keep the disk
/// busy, few enough that a slow disk pushes back on the socket.
const WRITE_QUEUE: usize = 8;

/// The temp file's marker, between the name and the random suffix.
const TEMP_MARKER: &str = ".fdupload-";

/// The longest prefix of the final name a temp name keeps, so the temp name
/// still fits a 255-byte file name.
const TEMP_NAME_PREFIX_MAX: usize = 200;

/// Why a request was refused or failed. The download route answers in the
/// same shape (`download_route.rs`).
pub(super) struct Failure {
    status: StatusCode,
    reason: &'static str,
    message: String,
}

impl Failure {
    pub(super) fn new(
        status: StatusCode,
        reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Failure {
            status,
            reason,
            message: message.into(),
        }
    }

    /// The stable code, for a log line.
    pub(super) fn reason(&self) -> &'static str {
        self.reason
    }

    pub(super) fn response(&self) -> Response {
        json_response(
            self.status,
            serde_json::json!({
                "ok": false,
                "reason": self.reason,
                "message": self.message,
            }),
        )
    }

    fn io(e: &std::io::Error) -> Failure {
        // `StorageFull` is unstable to match on by kind; ENOSPC is ENOSPC.
        if e.raw_os_error() == Some(libc::ENOSPC) {
            return Failure::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "no_space",
                "The worker ran out of disk space.",
            );
        }
        Failure::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "write_failed",
            format!("The worker could not write the file: {e}"),
        )
    }
}

/// A spent grant, from redemption to its one [`UploadFinished`]. Dropped
/// unfinished — a refusal, a timeout, the request future dropped because
/// the client hung up — it removes the temp file and reports the failure.
struct Receipt {
    shared: Arc<Shared>,
    grant: UploadGrant,
    temp: Option<PathBuf>,
    received: u64,
    reported: bool,
}

impl Receipt {
    fn report(&mut self, finished: UploadFinished) {
        if !self.reported {
            self.reported = true;
            self.shared.notify(WebInbound::UploadFinished(finished));
        }
    }

    fn fail(&mut self, failure: &Failure) {
        let finished = self.grant.failed(failure.message.clone(), self.received);
        self.report(finished);
    }
}

impl Drop for Receipt {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            remove_soon(temp);
        }
        if !self.reported {
            debuglog::log(&format!(
                "web UPLOAD interrupted batch={} name={}",
                self.grant.batch, self.grant.final_name
            ));
            let finished = self
                .grant
                .failed("The upload was interrupted.", self.received);
            self.report(finished);
        }
    }
}

/// Remove `path` off the async threads when there is a runtime to do it on.
fn remove_soon(path: PathBuf) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || {
                let _ = std::fs::remove_file(&path);
            });
        }
        Err(_) => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// `PUT /upload/{ticket}`. See the module docs.
pub(super) async fn upload_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(conn): ConnectInfo<PeerInfo>,
    UrlPath(ticket): UrlPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let address = rate_limit_address(conn.addr);
    let token = match authenticate(&shared, conn.addr, &headers) {
        Ok(token) => token,
        Err(failure) => {
            debuglog::log(&format!(
                "web UPLOAD refused address={address} reason={}",
                failure.as_str()
            ));
            let attempts = shared
                .credentials
                .unwrap_or_recover()
                .attempts_remaining(&address);
            return refusal_response(failure, attempts, shared.now_ms());
        }
    };
    let length = match declared_length(&headers) {
        Ok(length) => length,
        Err(failure) => return failure.response(),
    };

    let (verdict, expired) =
        shared
            .uploads
            .unwrap_or_recover()
            .redeem(&ticket, &token, length, shared.now_ms());
    shared.report_expired_uploads(expired);
    let grant = match verdict {
        Redeem::Granted(grant) => grant,
        Redeem::Invalid => {
            return Failure::new(
                StatusCode::FORBIDDEN,
                "invalid_ticket",
                "This upload is not (or no longer) allowed. Start it again.",
            )
            .response();
        }
        Redeem::SizeMismatch { expected } => {
            return Failure::new(
                StatusCode::BAD_REQUEST,
                "size_mismatch",
                format!("The upload is {length} bytes; {expected} were announced."),
            )
            .response();
        }
    };

    let mut receipt = Receipt {
        shared: Arc::clone(&shared),
        grant,
        temp: None,
        received: 0,
        reported: false,
    };
    match receive(&mut receipt, body).await {
        Ok(path) => {
            let bytes = receipt.grant.size;
            debuglog::log(&format!(
                "web UPLOAD done batch={} bytes={bytes}",
                receipt.grant.batch
            ));
            receipt.report(UploadFinished {
                token: receipt.grant.token.clone(),
                batch: receipt.grant.batch,
                path: path.clone(),
                bytes,
                ok: true,
                reason: None,
            });
            json_response(
                StatusCode::OK,
                serde_json::json!({
                    "ok": true,
                    "path": path.display().to_string(),
                    "bytes": bytes,
                }),
            )
        }
        Err(failure) => {
            receipt.fail(&failure);
            failure.response()
        }
    }
}

/// The request's `Content-Length`. Chunked bodies are refused: the size is
/// part of the grant, so it must be stated up front.
fn declared_length(headers: &HeaderMap) -> Result<u64, Failure> {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            "bad_length",
            "Send the file with a Content-Length, not chunked.",
        ));
    }
    let Some(value) = headers.get(header::CONTENT_LENGTH) else {
        return Err(Failure::new(
            StatusCode::LENGTH_REQUIRED,
            "length_required",
            "An upload must say its length.",
        ));
    };
    value
        .to_str()
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_REQUEST,
                "bad_length",
                "The upload's Content-Length is not a number.",
            )
        })
}

/// Receive the body into a temp file and commit it. Returns the final path.
async fn receive(receipt: &mut Receipt, body: Body) -> Result<PathBuf, Failure> {
    let grant = receipt.grant.clone();

    // The folder must still be the one granted, before anything is created.
    // If the request is dropped (the client hung up) while the temp is being
    // made, nobody is left to receive it, and the blocking task removes it
    // itself: no window in which a temp exists that no one owns.
    let (file, temp) = {
        let grant = grant.clone();
        let (made_tx, made_rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let made = check_dir(&grant).and_then(|()| create_temp(&grant));
            if let Err(Ok((_, orphan))) = made_tx.send(made) {
                let _ = std::fs::remove_file(orphan);
            }
        });
        made_rx.await.unwrap_or_else(|_| Err(writer_gone()))?
    };
    receipt.temp = Some(temp.clone());

    let (tx, mut rx) = mpsc::channel::<axum::body::Bytes>(WRITE_QUEUE);
    let writer = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut file = file;
        while let Some(chunk) = rx.blocking_recv() {
            file.write_all(&chunk)?;
        }
        file.sync_all()
    });

    let mut stream = body.into_data_stream();
    let mut cut_short = None;
    loop {
        let next = match tokio::time::timeout(IDLE_READ, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                cut_short = Some(Failure::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "timeout",
                    "The upload stalled for a minute and was stopped.",
                ));
                break;
            }
        };
        match next {
            None => break,
            Some(Err(_)) => {
                cut_short = Some(incomplete(receipt.received, grant.size));
                break;
            }
            Some(Ok(chunk)) => {
                receipt.received = receipt.received.saturating_add(chunk.len() as u64);
                if receipt.received > grant.size {
                    cut_short = Some(Failure::new(
                        StatusCode::BAD_REQUEST,
                        "too_many_bytes",
                        format!("The upload ran past the {} bytes announced.", grant.size),
                    ));
                    break;
                }
                // A closed queue means the writer failed; its error follows.
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(tx);
    let written = writer.await;
    if let Some(failure) = cut_short {
        return Err(failure);
    }
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(Failure::io(&e)),
        Err(_) => return Err(writer_gone()),
    }
    if receipt.received != grant.size {
        return Err(incomplete(receipt.received, grant.size));
    }

    let path = run_blocking(move || commit(&grant, &temp)).await?;
    // Committed: the temp is gone (renamed or unlinked).
    receipt.temp = None;
    Ok(path)
}

fn incomplete(received: u64, size: u64) -> Failure {
    Failure::new(
        StatusCode::BAD_REQUEST,
        "incomplete",
        format!("The upload stopped after {received} of {size} bytes."),
    )
}

/// Run blocking filesystem work off the async threads.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| Err(writer_gone()))
}

/// A blocking task that panicked or was cancelled.
fn writer_gone() -> Failure {
    Failure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "write_failed",
        "The worker's writer stopped unexpectedly.",
    )
}

/// The granted folder, canonicalised now, must still be itself and inside
/// `$HOME` — a folder swapped for a symlink since the grant is refused.
fn check_dir(grant: &UploadGrant) -> Result<(), Failure> {
    let moved = || {
        Failure::new(
            StatusCode::CONFLICT,
            "folder_changed",
            "The target folder changed on the worker. Choose it again.",
        )
    };
    let canonical = std::fs::canonicalize(&grant.dir).map_err(|_| moved())?;
    if canonical != grant.dir || !canonical.starts_with(&grant.home) || !canonical.is_dir() {
        return Err(moved());
    }
    Ok(())
}

/// A new hidden temp file beside the final name.
fn create_temp(grant: &UploadGrant) -> Result<(std::fs::File, PathBuf), Failure> {
    let prefix = truncate_on_char(&grant.final_name, TEMP_NAME_PREFIX_MAX);
    for _ in 0..8 {
        let mut suffix = [0u8; 6];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut suffix);
        let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let temp = grant.dir.join(format!(".{prefix}{TEMP_MARKER}{hex}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((file, temp)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Failure::io(&e)),
        }
    }
    Err(Failure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "write_failed",
        "The worker could not create a temporary file.",
    ))
}

/// At most `max` bytes of `text`, cut on a character boundary.
fn truncate_on_char(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Put the written temp file at its final name.
fn commit(grant: &UploadGrant, temp: &Path) -> Result<PathBuf, Failure> {
    check_dir(grant)?;
    let final_path = grant.final_path();
    let taken = || {
        Failure::new(
            StatusCode::CONFLICT,
            "exists",
            format!(
                "{} appeared on the worker meanwhile; it was left alone.",
                grant.final_name
            ),
        )
    };
    if grant.replace {
        if final_path.is_dir() {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "exists",
                format!("{} is a folder on the worker.", grant.final_name),
            ));
        }
        std::fs::rename(temp, &final_path).map_err(|e| Failure::io(&e))?;
        return Ok(final_path);
    }
    match std::fs::hard_link(temp, &final_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(temp);
            Ok(final_path)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(taken()),
        // A filesystem without hard links: claim the name only if it is
        // still free, then move the temp onto it (a narrower race, same
        // outcome when nothing else writes there).
        Err(_) => {
            if std::fs::symlink_metadata(&final_path).is_ok() {
                return Err(taken());
            }
            std::fs::rename(temp, &final_path).map_err(|e| Failure::io(&e))?;
            Ok(final_path)
        }
    }
}
