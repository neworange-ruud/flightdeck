//! `PUT /upload/{ticket}`: sending one file's bytes to a worker (v8,
//! `remote-control-usem`).
//!
//! The shared hand-rolled HTTP/1.1 of [`super::http`] — plain `http://`, the
//! link's own `Cookie: flightdeck_web=…` and `User-Agent` — with the body a
//! local file, streamed in
//! [`CHUNK_BYTES`] chunks read on a blocking thread, never held whole.
//! `Content-Length` is the size the worker granted; a file that changed size
//! since it was picked stops the upload rather than send a different length.
//!
//! [`put_file`] is the request; [`Transfer`] runs one on the shared runtime
//! for a synchronous front-end, reports [`TransferEvent`]s on a channel, and
//! is cancelled by dropping it (or [`Transfer::abort`]): the task stops and
//! the connection closes, and the worker removes what it had written.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::web::server::UPLOAD_PATH;

use super::exchange::parse_response;
use super::http::{access_sentence, connect, refusal_parts, request_head};
use super::upload::UploadJob;

pub use super::http::HttpAccess;

/// How much of the file one write carries.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// How many chunks the reader may get ahead of the socket.
const READ_AHEAD: usize = 4;

/// The longest one chunk may take to go out before the worker counts as gone.
const WRITE_STALL: Duration = Duration::from_secs(60);

/// How long the worker may take to answer once every byte is out (it
/// `fsync`s first).
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// How often progress is reported, at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// The largest reply read. It is a line of JSON.
const MAX_REPLY: u64 = 64 * 1024;

/// What the worker said when it took the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadReply {
    /// Where the file is on the worker.
    pub path: String,
    /// How many bytes it wrote.
    pub bytes: u64,
}

/// Why a file did not get there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The worker answered and refused: its status, its `reason` code, and
    /// its sentence when it gave one.
    Refused {
        status: u16,
        reason: String,
        message: Option<String>,
    },
    /// The file here could not be read, or changed size since it was picked.
    Local(String),
    /// The worker could not be reached, stalled, or hung up without a word.
    Network(String),
}

impl UploadError {
    /// One sentence for the panel.
    pub fn sentence(&self) -> String {
        match self {
            UploadError::Refused {
                message: Some(message),
                ..
            } => message.clone(),
            UploadError::Refused { reason, status, .. } => match access_sentence(reason) {
                Some(sentence) => sentence.to_string(),
                None => format!("The worker refused the file (HTTP {status}, {reason})."),
            },
            UploadError::Local(why) => why.clone(),
            UploadError::Network(why) => format!("The upload did not reach the worker: {why}."),
        }
    }
}

/// `PUT` the first `size` bytes of the file at `path` to `ticket`, calling
/// `progress` with the bytes sent so far (at most every [`PROGRESS_EVERY`],
/// and once at the end).
pub async fn put_file(
    access: &HttpAccess,
    ticket: &str,
    path: &Path,
    size: u64,
    mut progress: impl FnMut(u64) + Send,
) -> Result<UploadReply, UploadError> {
    let mut stream = connect(&access.address)
        .await
        .map_err(UploadError::Network)?;
    let head = request_head(
        "PUT",
        &format!("{UPLOAD_PATH}/{ticket}"),
        access,
        &[
            ("Content-Type", "application/octet-stream".to_string()),
            ("Content-Length", size.to_string()),
        ],
    );
    let net = |e: std::io::Error| UploadError::Network(e.to_string());
    stream.write_all(head.as_bytes()).await.map_err(net)?;

    // The file is read on a blocking thread, a few chunks ahead of the socket.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(READ_AHEAD);
    let local = path.to_path_buf();
    let reader = tokio::task::spawn_blocking(move || read_chunks(&local, size, &tx));

    let mut sent: u64 = 0;
    let mut last_report = Instant::now();
    let mut cut_off = None;
    while let Some(chunk) = rx.recv().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(why) => {
                // Stop here: the worker sees a short body and drops the file.
                drop(stream);
                let _ = reader.await;
                return Err(UploadError::Local(why));
            }
        };
        match tokio::time::timeout(WRITE_STALL, stream.write_all(&chunk)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                cut_off = Some(net(e));
                break;
            }
            Err(_) => {
                return Err(UploadError::Network(
                    "the worker stopped taking bytes".to_string(),
                ))
            }
        }
        sent += chunk.len() as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            progress(sent);
        }
    }
    drop(rx);
    let _ = reader.await;
    if cut_off.is_none() {
        progress(sent);
    }

    // The worker may already have answered (a refusal before the body is
    // done): read whatever it said, either way.
    let mut raw = Vec::new();
    let read = tokio::time::timeout(
        REPLY_TIMEOUT,
        (&mut stream).take(MAX_REPLY).read_to_end(&mut raw),
    )
    .await;
    if raw.is_empty() {
        return Err(match (cut_off, read) {
            (Some(e), _) => e,
            (None, Err(_)) => UploadError::Network("the worker did not answer".to_string()),
            (None, Ok(Err(e))) => net(e),
            (None, Ok(Ok(_))) => UploadError::Network("the worker closed without answering".into()),
        });
    }
    reply(&raw)
}

/// Read exactly `size` bytes of `path` into the channel, chunk by chunk. A
/// file that ends early or runs on is an error: it changed since it was
/// picked, and the worker was promised `size`.
fn read_chunks(path: &Path, size: u64, tx: &tokio::sync::mpsc::Sender<Result<Vec<u8>, String>>) {
    use std::io::Read;
    let changed = || format!("{} changed since it was picked; pick it again.", name(path));
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => {
            let _ = tx.blocking_send(Err(format!("Could not read {}: {e}", name(path))));
            return;
        }
    };
    let mut left = size;
    let mut buf = vec![0u8; CHUNK_BYTES];
    while left > 0 {
        let want = (left as usize).min(CHUNK_BYTES);
        let n = match file.read(&mut buf[..want]) {
            Ok(0) => {
                let _ = tx.blocking_send(Err(changed()));
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = tx.blocking_send(Err(format!("Could not read {}: {e}", name(path))));
                return;
            }
        };
        left -= n as u64;
        // Before the last chunk goes, one byte more means the file grew: the
        // last chunk is withheld, so the worker never completes a cut copy.
        if left == 0 {
            let mut probe = [0u8; 1];
            if matches!(file.read(&mut probe), Ok(more) if more > 0) {
                let _ = tx.blocking_send(Err(changed()));
                return;
            }
        }
        if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
            return;
        }
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The worker's JSON answer.
fn reply(raw: &[u8]) -> Result<UploadReply, UploadError> {
    let response = parse_response(raw).map_err(|e| UploadError::Network(e.sentence()))?;
    let body: serde_json::Value =
        serde_json::from_str(&response.body).unwrap_or(serde_json::Value::Null);
    if response.status == 200 && body["ok"] == serde_json::Value::Bool(true) {
        return Ok(UploadReply {
            path: body["path"].as_str().unwrap_or_default().to_string(),
            bytes: body["bytes"].as_u64().unwrap_or(0),
        });
    }
    let (reason, message) = refusal_parts(&response.body);
    Err(UploadError::Refused {
        status: response.status,
        reason,
        message,
    })
}

/// What a running [`Transfer`] says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferEvent {
    /// `sent` bytes of item `index` are out.
    Progress { index: usize, sent: u64 },
    /// Item `index` ended.
    Finished {
        index: usize,
        result: Result<UploadReply, UploadError>,
    },
}

/// One file going out on the shared runtime. Dropping it (or
/// [`Transfer::abort`]) stops it and closes the connection; no
/// [`TransferEvent::Finished`] follows then.
pub struct Transfer {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Transfer {
    /// Start sending `job` with `access`, reporting on `events`.
    pub fn start(access: HttpAccess, job: UploadJob, events: Sender<TransferEvent>) -> Transfer {
        let index = job.index;
        let Some(runtime) = crate::remote::runtime::try_shared() else {
            let _ = events.send(TransferEvent::Finished {
                index,
                result: Err(UploadError::Network("no network runtime".to_string())),
            });
            return Transfer { task: None };
        };
        // `Sender` is not `Sync`; the task's future must be `Send`.
        let events = Arc::new(Mutex::new(events));
        let task = runtime.spawn(async move {
            let send = |event: TransferEvent| {
                if let Ok(tx) = events.lock() {
                    let _ = tx.send(event);
                }
            };
            let local: PathBuf = job.local.clone();
            let result = put_file(&access, &job.ticket, &local, job.size, |sent| {
                send(TransferEvent::Progress { index, sent })
            })
            .await;
            send(TransferEvent::Finished { index, result });
        });
        Transfer { task: Some(task) }
    }

    /// Stop it now.
    pub fn abort(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_file_is_read_from_the_answer() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"ok\":true,\"path\":\"/home/u/a.png\",\"bytes\":5}";
        assert_eq!(
            reply(raw),
            Ok(UploadReply {
                path: "/home/u/a.png".to_string(),
                bytes: 5
            })
        );
    }

    #[test]
    fn a_refusal_keeps_the_workers_code_and_sentence() {
        let raw = b"HTTP/1.1 409 Conflict\r\n\r\n{\"ok\":false,\"reason\":\"exists\",\"message\":\"a.png appeared on the worker meanwhile; it was left alone.\"}";
        let error = reply(raw).unwrap_err();
        assert_eq!(
            error,
            UploadError::Refused {
                status: 409,
                reason: "exists".to_string(),
                message: Some(
                    "a.png appeared on the worker meanwhile; it was left alone.".to_string()
                ),
            }
        );
        assert!(error.sentence().starts_with("a.png appeared"));
    }

    #[test]
    fn an_auth_refusal_without_a_sentence_is_still_worded() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"ok\":false,\"reason\":\"token_revoked\"}";
        assert_eq!(
            reply(raw).unwrap_err().sentence(),
            "The worker withdrew this app's access."
        );
    }

    #[test]
    fn a_file_is_read_in_chunks_and_must_keep_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let data: Vec<u8> = (0..(CHUNK_BYTES * 2 + 10)).map(|i| i as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let read = |size: u64| {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
            read_chunks(&path, size, &tx);
            drop(tx);
            let mut out = Vec::new();
            while let Ok(item) = rx.try_recv() {
                out.push(item);
            }
            out
        };
        let chunks = read(data.len() as u64);
        assert_eq!(chunks.len(), 3);
        let joined: Vec<u8> = chunks.into_iter().flat_map(|c| c.unwrap()).collect();
        assert_eq!(joined, data);
        // Granted fewer bytes than the file holds: it grew since it was
        // picked, and the last chunk is never sent.
        let grown = read(data.len() as u64 - 1);
        assert!(matches!(grown.last(), Some(Err(why)) if why.contains("changed")));
        assert_eq!(grown.len(), 3, "two full chunks, then the refusal");
        // Granted more: it shrank.
        let shrunk = read(data.len() as u64 + 1);
        assert!(matches!(shrunk.last(), Some(Err(why)) if why.contains("changed")));
        let missing = {
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            read_chunks(&dir.path().join("nope"), 1, &tx);
            rx.try_recv().unwrap()
        };
        assert!(matches!(missing, Err(why) if why.contains("Could not read nope")));
    }
}
