//! `GET /download/{ticket}`: fetching one file's bytes from a worker onto
//! this machine (v9, `remote-control-pz6d`).
//!
//! The shared hand-rolled HTTP/1.1 of [`super::http`] — plain `http://`, the
//! link's own cookie and agent — with the body streamed to disk in
//! [`CHUNK_BYTES`] chunks, written on a blocking thread, never held whole.
//!
//! The bytes go into a hidden temporary file beside the destination,
//! `.<name>.fddownload-<random>`, which is `fsync`ed and only then committed
//! under its real name:
//!
//! - **Replace**: renamed over whatever has the name.
//! - **Keep both / no clash**: the name is claimed only if it is still free
//!   — a hard link, then the temporary file unlinked; where hard links are
//!   not to be had, a rename after checking the name is free — and a file
//!   that appeared meanwhile makes it the next free `name (n).ext` instead.
//!   Nothing is ever overwritten that the person did not say to replace.
//!
//! The worker's `Content-Length` is the length (the file's size when it was
//! opened, which wins over the plan's), and a body that ends short of it is
//! a failure. On any failure or cancel the temporary file is removed (a drop
//! guard on the writing thread), so a partial file never lands. A refusal's
//! JSON `message` is what the person reads.
//!
//! [`get_file`] is the request; [`DownloadTransfer`] runs one on the shared
//! runtime for a synchronous front-end, reports [`DownloadEvent`]s on a
//! channel, and is cancelled by dropping it (or
//! [`DownloadTransfer::abort`]).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::host::worker::upload::keep_both_name;

use super::download::DownloadJob;
use super::http::{access_sentence, connect, read_head, refusal_parts, request_head, HttpAccess};

/// The route a worker serves downloads on: `GET {DOWNLOAD_PATH}/{ticket}`.
pub const DOWNLOAD_PATH: &str = "/download";

/// How much of the body one read carries.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// What a temporary file's name carries between the file's name and its
/// random tail: `.<name>.fddownload-<hex>`.
pub const TEMP_MARK: &str = ".fddownload-";

/// How many chunks the socket may get ahead of the disk.
const WRITE_AHEAD: usize = 4;

/// How long the worker may take to start answering (it opens the file).
const HEAD_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest the body may stall before the worker counts as gone.
const READ_STALL: Duration = Duration::from_secs(60);

/// How often progress is reported, at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// The largest refusal body read. It is a line of JSON.
const MAX_REPLY: u64 = 64 * 1024;

/// The most numbered names tried for one file before giving up.
const MAX_KEEP_BOTH: u32 = 10_000;

/// Where a file landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downloaded {
    /// Where it is on this machine.
    pub path: PathBuf,
    /// How many bytes it holds.
    pub bytes: u64,
}

/// Why a file did not land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// The worker answered and refused: its status, its `reason` code, and
    /// its sentence when it gave one.
    Refused {
        status: u16,
        reason: String,
        message: Option<String>,
    },
    /// The file could not be written here.
    Local(String),
    /// The worker could not be reached, stalled, or hung up early.
    Network(String),
}

impl DownloadError {
    /// One sentence for the panel.
    pub fn sentence(&self) -> String {
        match self {
            DownloadError::Refused {
                message: Some(message),
                ..
            } => message.clone(),
            DownloadError::Refused { reason, status, .. } => match access_sentence(reason) {
                Some(sentence) => sentence.to_string(),
                None => format!("The worker refused the file (HTTP {status}, {reason})."),
            },
            DownloadError::Local(why) => why.clone(),
            DownloadError::Network(why) => {
                format!("The download did not finish: {why}.")
            }
        }
    }
}

/// What the writing thread is told.
enum ToDisk {
    /// The next bytes.
    Chunk(Vec<u8>),
    /// Every byte is in: fsync and commit.
    Commit,
}

/// `GET` `job.ticket` into `job.dest` as `job.final_name`, calling
/// `progress(received, total)` (at most every [`PROGRESS_EVERY`], and once
/// at the end).
pub async fn get_file(
    access: &HttpAccess,
    job: &DownloadJob,
    mut progress: impl FnMut(u64, u64) + Send,
) -> Result<Downloaded, DownloadError> {
    for name in [&job.name, &job.final_name] {
        if !is_plain_name(name) {
            return Err(DownloadError::Local(format!(
                "\"{name}\" is not a file name this computer can save."
            )));
        }
    }
    let mut stream = connect(&access.address)
        .await
        .map_err(DownloadError::Network)?;
    let head = request_head(
        "GET",
        &format!("{DOWNLOAD_PATH}/{}", job.ticket),
        access,
        &[("Accept", "application/octet-stream".to_string())],
    );
    let net = |e: std::io::Error| DownloadError::Network(e.to_string());
    stream.write_all(head.as_bytes()).await.map_err(net)?;
    let (head, first) = read_head(&mut stream, HEAD_TIMEOUT)
        .await
        .map_err(DownloadError::Network)?;

    if head.status != 200 {
        // A refusal: its JSON, as much as was sent (or fits).
        let want = head.content_length().unwrap_or(MAX_REPLY).min(MAX_REPLY);
        let mut raw = first;
        if (raw.len() as u64) < want {
            let mut rest = Vec::new();
            let _ = tokio::time::timeout(
                READ_STALL,
                (&mut stream)
                    .take(want - raw.len() as u64)
                    .read_to_end(&mut rest),
            )
            .await;
            raw.extend_from_slice(&rest);
        }
        let (reason, message) = refusal_parts(&String::from_utf8_lossy(&raw));
        return Err(DownloadError::Refused {
            status: head.status,
            reason,
            message,
        });
    }
    let Some(total) = head.content_length() else {
        return Err(DownloadError::Network(
            "the worker did not say how long the file is".to_string(),
        ));
    };
    if first.len() as u64 > total {
        return Err(DownloadError::Network(
            "the worker sent more than the file's length".to_string(),
        ));
    }

    // The disk is written on a blocking thread, a few chunks behind the
    // socket. Hanging up on it without `Commit` removes the temporary file.
    let (tx, rx) = tokio::sync::mpsc::channel::<ToDisk>(WRITE_AHEAD);
    let target = Target {
        dir: job.dest.clone(),
        name: job.name.clone(),
        final_name: job.final_name.clone(),
        replace: job.replace,
    };
    let writer = tokio::task::spawn_blocking(move || write_file(&target, total, rx));

    let mut received = first.len() as u64;
    let mut cut_short = None;
    if !first.is_empty() && tx.send(ToDisk::Chunk(first)).await.is_err() {
        return finish_writer(writer).await;
    }
    progress(received, total);
    let mut last_report = Instant::now();
    let mut reported = received;
    let mut waited = Duration::ZERO;
    let mut buf = vec![0u8; CHUNK_BYTES];
    while received < total {
        let want = ((total - received) as usize).min(CHUNK_BYTES);
        // Read in short waits, so a body that pauses still shows how far it
        // got; [`READ_STALL`] of them in a row and the worker is gone.
        let n = match tokio::time::timeout(PROGRESS_EVERY, stream.read(&mut buf[..want])).await {
            Err(_) => {
                if reported != received {
                    reported = received;
                    last_report = Instant::now();
                    progress(received, total);
                }
                waited += PROGRESS_EVERY;
                if waited >= READ_STALL {
                    cut_short = Some("the worker stopped sending".to_string());
                    break;
                }
                continue;
            }
            Ok(Ok(0)) => {
                cut_short = Some(format!(
                    "the worker hung up after {received} of {total} bytes"
                ));
                break;
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                cut_short = Some(e.to_string());
                break;
            }
        };
        waited = Duration::ZERO;
        if tx.send(ToDisk::Chunk(buf[..n].to_vec())).await.is_err() {
            // The writer stopped (the disk refused): its reason is the one.
            return finish_writer(writer).await;
        }
        received += n as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            reported = received;
            progress(received, total);
        }
    }
    if let Some(why) = cut_short {
        drop(tx);
        let _ = writer.await;
        return Err(DownloadError::Network(why));
    }
    progress(received, total);
    if tx.send(ToDisk::Commit).await.is_err() {
        return finish_writer(writer).await;
    }
    drop(tx);
    finish_writer(writer).await
}

async fn finish_writer(
    writer: tokio::task::JoinHandle<Result<Downloaded, String>>,
) -> Result<Downloaded, DownloadError> {
    match writer.await {
        Ok(result) => result.map_err(DownloadError::Local),
        Err(e) => Err(DownloadError::Local(e.to_string())),
    }
}

/// Whether `name` is one plain file name: not empty, not `.`/`..`, no
/// separator of any platform, no NUL.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).file_name().and_then(|n| n.to_str()) == Some(name)
}

/// Where one file lands.
#[derive(Clone, Debug)]
struct Target {
    /// The folder.
    dir: PathBuf,
    /// Its name on the worker: the base for a numbered name.
    name: String,
    /// The name to save it as.
    final_name: String,
    /// Overwrite a file of that name.
    replace: bool,
}

/// Removes a temporary file when dropped, unless disarmed.
struct TempGuard(Option<PathBuf>);

impl TempGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A fresh temporary file for `name` in `dir`: `.<name>.fddownload-<hex>`,
/// created new (never an existing file).
fn create_temp(dir: &Path, name: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
    use rand_core::RngCore;
    let mut last = None;
    for _ in 0..8 {
        let tail = rand_core::OsRng.next_u64();
        let path = dir.join(format!(".{name}{TEMP_MARK}{tail:016x}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary name")))
}

/// The writing thread: a temporary file in the target's folder, the chunks
/// as they come, then — on `Commit`, with exactly `total` bytes — fsync and
/// commit. Anything else (the socket side hung up, the disk refused, the
/// length is off) removes the temporary file.
fn write_file(
    target: &Target,
    total: u64,
    mut rx: tokio::sync::mpsc::Receiver<ToDisk>,
) -> Result<Downloaded, String> {
    let shown = |e: std::io::Error| {
        format!(
            "Could not save {} in {}: {e}",
            target.final_name,
            target.dir.display()
        )
    };
    let (temp, mut file) = create_temp(&target.dir, &target.final_name).map_err(shown)?;
    let mut guard = TempGuard(Some(temp.clone()));
    let mut written: u64 = 0;
    loop {
        match rx.blocking_recv() {
            Some(ToDisk::Chunk(bytes)) => {
                file.write_all(&bytes).map_err(shown)?;
                written += bytes.len() as u64;
            }
            Some(ToDisk::Commit) => break,
            None => return Err("Cancelled.".to_string()),
        }
    }
    if written != total {
        return Err(format!(
            "{} arrived with {written} of {total} bytes.",
            target.final_name
        ));
    }
    file.sync_all().map_err(shown)?;
    drop(file);
    let path = commit(&temp, target).map_err(shown)?;
    guard.disarm();
    sync_dir(&target.dir);
    Ok(Downloaded {
        path,
        bytes: written,
    })
}

/// Give the temporary file its name: renamed over the target (Replace), or
/// the target claimed only while free, and the next free numbered name if a
/// file took it meanwhile. Returns where it landed.
fn commit(temp: &Path, target: &Target) -> std::io::Result<PathBuf> {
    let first = target.dir.join(&target.final_name);
    if target.replace {
        std::fs::rename(temp, &first)?;
        return Ok(first);
    }
    match claim(temp, &first) {
        Ok(()) => return Ok(first),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    for n in 1..=MAX_KEEP_BOTH {
        let candidate = keep_both_name(&target.name, n);
        if candidate == target.final_name {
            continue;
        }
        let path = target.dir.join(&candidate);
        match claim(temp, &path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free name left",
    ))
}

/// Move `temp` to `path` only if `path` is free: a hard link (atomic: it
/// fails on a taken name) then unlink the temporary file; where the folder
/// takes no hard links, a rename after checking the name is free.
fn claim(temp: &Path, path: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(temp, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(temp);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(temp, path)
        }
    }
}

/// Make a rename in `dir` durable where the platform lets a folder be
/// synced (best effort).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// What a running [`DownloadTransfer`] says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    /// `received` of `total` bytes of item `index` are in.
    Progress {
        index: usize,
        received: u64,
        total: u64,
    },
    /// Item `index` ended.
    Finished {
        index: usize,
        result: Result<Downloaded, DownloadError>,
    },
}

/// One file coming in on the shared runtime. Dropping it (or
/// [`DownloadTransfer::abort`]) stops it and closes the connection, and the
/// temporary file is removed; no [`DownloadEvent::Finished`] follows then.
pub struct DownloadTransfer {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl DownloadTransfer {
    /// Start fetching `job` with `access`, reporting on `events`.
    pub fn start(
        access: HttpAccess,
        job: DownloadJob,
        events: Sender<DownloadEvent>,
    ) -> DownloadTransfer {
        let index = job.index;
        let Some(runtime) = crate::remote::runtime::try_shared() else {
            let _ = events.send(DownloadEvent::Finished {
                index,
                result: Err(DownloadError::Network("no network runtime".to_string())),
            });
            return DownloadTransfer { task: None };
        };
        // `Sender` is not `Sync`; the task's future must be `Send`.
        let events = Arc::new(Mutex::new(events));
        let task = runtime.spawn(async move {
            let send = |event: DownloadEvent| {
                if let Ok(tx) = events.lock() {
                    let _ = tx.send(event);
                }
            };
            let result = get_file(&access, &job, |received, total| {
                send(DownloadEvent::Progress {
                    index,
                    received,
                    total,
                })
            })
            .await;
            send(DownloadEvent::Finished { index, result });
        });
        DownloadTransfer { task: Some(task) }
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

impl Drop for DownloadTransfer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
