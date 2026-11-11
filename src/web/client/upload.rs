//! Uploading files to a worker, as a pure reducer (v8, `remote-control-usem`).
//!
//! The person picks files on this machine and a folder on the worker, and
//! asks; the worker answers [`begin_upload`](super::RemoteClient::begin_upload)
//! with an [`UploadPlan`] — a ticket per file, the names already taken, or a
//! refusal — and the files then go one at a time over
//! [`super::upload_http`]. [`UploadPanel`] holds what the panel shows and
//! folds all of that in:
//!
//! ```text
//! Choosing ──upload──▶ Requested(seq) ──plan: tickets──▶ Uploading ──▶ Done
//!    ▲                    │   │                              │
//!    │    refusal / error │   └─plan: conflicts─▶ Asking ────┤ replace / keep both
//!    └────────────────────┘                        │  cancel │  (re-request)
//!                       Cancelled ◀── cancel (any phase) ────┘
//! ```
//!
//! It sends nothing and touches no file: the front-end (through
//! [`super::session::RemoteSession`]) sends what [`UploadPanel::request`]
//! returns, tells the panel the seq it got ([`UploadPanel::requested`]),
//! starts the transfer [`UploadPanel::next_job`] names, and feeds back the
//! plan, the acks and errors, and the transfer's progress and end. An answer
//! to anything but the latest request is dropped.

use std::path::{Path, PathBuf};

use crate::web::protocol::{Ack, AckOutcome, OnConflict, UploadFile, UploadPlan, WireError};

use super::mirror::RemoteWorkspace;
use super::worker::NOT_SENT;

/// How many of its own seqs the panel remembers, so a superseded request's
/// ack is still recognised as the panel's.
const SEQS_KEPT: usize = 16;

/// Said when a picked item is a folder.
pub const FOLDER_REFUSED: &str = "Folders cannot be uploaded; choose the files inside.";

/// Said when an upload is asked for with no files.
pub const NO_FILES: &str = "Choose at least one file to upload.";

/// Said when the link dropped while the worker was being asked.
pub const LINK_LOST: &str = "The connection to the worker dropped; upload again.";

/// Said for a file the person cancelled before it was sent.
pub const CANCELLED: &str = "Cancelled.";

/// A file on this machine, as picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    /// Where it is here.
    pub path: PathBuf,
    /// Its name: what it is called on the worker unless that is taken.
    pub name: String,
    /// Its size in bytes when picked.
    pub size: u64,
}

/// One thing the person picked, as this machine sees it ([`inspect`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picked {
    /// A file: uploadable.
    File(LocalFile),
    /// A folder: refused with [`FOLDER_REFUSED`].
    Folder(PathBuf),
    /// Unreadable or gone, with why.
    Unreadable(PathBuf, String),
}

/// Look at `path` on this machine: a file (with its name and size), a
/// folder, or something that cannot be read.
pub fn inspect(path: &Path) -> Picked {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Picked::Folder(path.to_path_buf()),
        Ok(_) if name.is_empty() => Picked::Unreadable(
            path.to_path_buf(),
            format!("{} has no file name", path.display()),
        ),
        Ok(meta) => Picked::File(LocalFile {
            path: path.to_path_buf(),
            name,
            size: meta.len(),
        }),
        Err(e) => Picked::Unreadable(path.to_path_buf(), e.to_string()),
    }
}

/// The folder an upload goes to by default: the selected session's folder
/// (its worktree), else the selected project's root, else the worker's
/// `$HOME`, else `~`.
pub fn default_dir(workspace: &RemoteWorkspace) -> String {
    let session = workspace
        .selected_session()
        .map(|s| s.worktree_path.clone())
        .filter(|p| !p.is_empty());
    let project = || {
        workspace
            .selected_project()
            .map(|p| p.root.clone())
            .filter(|p| !p.is_empty())
    };
    let home = || {
        workspace
            .worker
            .as_ref()
            .map(|w| w.home.clone())
            .filter(|h| !h.is_empty())
    };
    session
        .or_else(project)
        .or_else(home)
        .unwrap_or_else(|| "~".to_string())
}

/// What to send: a `begin_upload`'s arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    /// The target folder, as typed.
    pub dir: String,
    /// The files, in list order.
    pub files: Vec<UploadFile>,
    /// What to do about taken names.
    pub on_conflict: OnConflict,
}

/// The person's answer to taken names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Overwrite them.
    Replace,
    /// Keep both: the uploads get numbered names.
    KeepBoth,
    /// Neither: back to the list.
    Cancel,
}

/// How one file of a granted upload stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemState {
    /// Not started.
    Waiting,
    /// Its bytes are going out now.
    Sending,
    /// On the worker, at this path.
    Written(String),
    /// Not on the worker, with why.
    Failed(String),
}

/// One file of a granted upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadItem {
    /// The name asked for.
    pub name: String,
    /// The name the worker gives it.
    pub final_name: String,
    /// Where it is on this machine.
    pub local: PathBuf,
    /// Its ticket.
    pub ticket: String,
    /// Bytes sent so far.
    pub sent: u64,
    /// Its size.
    pub total: u64,
    /// How it stands.
    pub state: ItemState,
}

/// One transfer to start: `PUT` the file at `local` to `ticket`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadJob {
    /// Which item, for [`UploadPanel::progress`] and [`UploadPanel::finished`].
    pub index: usize,
    /// The file here.
    pub local: PathBuf,
    /// Its ticket.
    pub ticket: String,
    /// Its exact size, as granted.
    pub size: u64,
}

/// Where the panel is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadPhase {
    /// Picking files and a folder.
    Choosing,
    /// A `begin_upload` is out as `seq`.
    Requested {
        /// Its seq.
        seq: u64,
        /// What it asked for taken names.
        on_conflict: OnConflict,
    },
    /// Names are taken; the person chooses ([`Resolution`]).
    Asking {
        /// The canonical target folder.
        dir: String,
        /// The names already there, in list order.
        conflicts: Vec<String>,
    },
    /// The files are going out, one at a time.
    Uploading {
        /// The canonical target folder.
        dir: String,
        /// Every granted file.
        items: Vec<UploadItem>,
        /// The one being sent, if any.
        current: Option<usize>,
    },
    /// Every file has been sent or has failed.
    Done {
        /// The canonical target folder.
        dir: String,
        /// The files written, by final name.
        written: Vec<String>,
        /// The ones that were not, with why.
        failed: Vec<(String, String)>,
    },
    /// The person cancelled.
    Cancelled,
}

/// The upload panel's state. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadPanel {
    /// The target folder on the worker, as typed.
    pub dir: String,
    /// The files picked, in order, one per name.
    pub files: Vec<LocalFile>,
    /// Where the panel is.
    pub phase: UploadPhase,
    /// The last thing worth saying: a refusal, a skipped pick.
    pub note: Option<String>,
    /// Every seq the panel sent lately, newest last.
    mine: Vec<u64>,
}

impl UploadPanel {
    /// A panel uploading into `dir`, with nothing picked yet.
    pub fn new(dir: impl Into<String>) -> UploadPanel {
        UploadPanel {
            dir: dir.into(),
            files: Vec::new(),
            phase: UploadPhase::Choosing,
            note: None,
            mine: Vec::new(),
        }
    }

    /// Whether the list and the folder may still change.
    pub fn is_choosing(&self) -> bool {
        self.phase == UploadPhase::Choosing
    }

    /// Whether a transfer is under way (or about to be).
    pub fn is_uploading(&self) -> bool {
        matches!(self.phase, UploadPhase::Uploading { .. })
    }

    /// The total size of the files picked.
    pub fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0u64, |sum, f| sum.saturating_add(f.size))
    }

    /// `(sent, total)` bytes over every granted file, while uploading.
    pub fn progress_bytes(&self) -> Option<(u64, u64)> {
        let UploadPhase::Uploading { items, .. } = &self.phase else {
            return None;
        };
        Some(items.iter().fold((0u64, 0u64), |(sent, total), item| {
            (
                sent.saturating_add(item.sent),
                total.saturating_add(item.total),
            )
        }))
    }

    // -- choosing -------------------------------------------------------------

    /// Add what the person picked. A name already in the list is skipped
    /// (one file per name), a folder is refused; either is said in the note.
    /// Ignored outside [`UploadPhase::Choosing`]. Returns whether anything
    /// changed.
    pub fn add(&mut self, picked: Vec<Picked>) -> bool {
        if !self.is_choosing() || picked.is_empty() {
            return false;
        }
        let mut notes = Vec::new();
        for pick in picked {
            match pick {
                Picked::File(file) => {
                    if self.files.iter().any(|f| f.name == file.name) {
                        notes.push(format!("{} is already in the list.", file.name));
                    } else {
                        self.files.push(file);
                    }
                }
                Picked::Folder(_) => {
                    if !notes.iter().any(|n| n == FOLDER_REFUSED) {
                        notes.push(FOLDER_REFUSED.to_string());
                    }
                }
                Picked::Unreadable(path, why) => {
                    notes.push(format!("Could not read {}: {why}", path.display()));
                }
            }
        }
        self.note = (!notes.is_empty()).then(|| notes.join(" "));
        true
    }

    /// Take the file at `index` out of the list.
    pub fn remove(&mut self, index: usize) -> bool {
        if !self.is_choosing() || index >= self.files.len() {
            return false;
        }
        self.files.remove(index);
        self.note = None;
        true
    }

    /// Upload into `dir` instead.
    pub fn set_dir(&mut self, dir: impl Into<String>) -> bool {
        if !self.is_choosing() {
            return false;
        }
        self.dir = dir.into();
        true
    }

    // -- asking the worker ----------------------------------------------------

    /// What to send for Upload: the list, the folder, and `ask` for taken
    /// names. `None` (with the note saying why) when there is nothing to send.
    pub fn request(&mut self) -> Option<UploadRequest> {
        if !self.is_choosing() {
            return None;
        }
        if self.files.is_empty() {
            self.note = Some(NO_FILES.to_string());
            return None;
        }
        Some(self.request_with(OnConflict::Ask))
    }

    fn request_with(&self, on_conflict: OnConflict) -> UploadRequest {
        UploadRequest {
            dir: self.dir.trim().to_string(),
            files: self
                .files
                .iter()
                .map(|f| UploadFile {
                    name: f.name.clone(),
                    size: f.size,
                })
                .collect(),
            on_conflict,
        }
    }

    /// The request went out as `seq` (`None`: it could not be sent).
    pub fn requested(&mut self, seq: Option<u64>, on_conflict: OnConflict) {
        match seq {
            None => {
                self.phase = UploadPhase::Choosing;
                self.note = Some(NOT_SENT.to_string());
            }
            Some(seq) => {
                self.mine.push(seq);
                let excess = self.mine.len().saturating_sub(SEQS_KEPT);
                self.mine.drain(..excess);
                self.phase = UploadPhase::Requested { seq, on_conflict };
                self.note = None;
            }
        }
    }

    /// The person's answer to taken names: a request to send again with that
    /// choice, or (`Cancel`) back to the list.
    pub fn resolve(&mut self, resolution: Resolution) -> Option<UploadRequest> {
        if !matches!(self.phase, UploadPhase::Asking { .. }) {
            return None;
        }
        match resolution {
            Resolution::Cancel => {
                self.phase = UploadPhase::Choosing;
                self.note = None;
                None
            }
            Resolution::Replace => Some(self.request_with(OnConflict::Replace)),
            Resolution::KeepBoth => Some(self.request_with(OnConflict::KeepBoth)),
        }
    }

    // -- the worker's answers ---------------------------------------------------

    /// The worker's plan. Dropped unless it answers the request out now.
    /// Returns whether the panel changed.
    pub fn on_plan(&mut self, plan: UploadPlan) -> bool {
        let UploadPhase::Requested { seq, .. } = self.phase else {
            return false;
        };
        if plan.seq != seq {
            return false;
        }
        if let Some(refusal) = plan.refusal {
            self.phase = UploadPhase::Choosing;
            self.note = Some(refusal);
            return true;
        }
        if plan.conflicts {
            self.phase = UploadPhase::Asking {
                dir: plan.dir,
                conflicts: plan
                    .files
                    .iter()
                    .filter(|f| f.exists)
                    .map(|f| f.name.clone())
                    .collect(),
            };
            return true;
        }
        let mut items = Vec::with_capacity(plan.files.len());
        for row in plan.files {
            let Some(file) = self.files.iter().find(|f| f.name == row.name) else {
                continue;
            };
            let Some(ticket) = row.ticket else {
                continue;
            };
            items.push(UploadItem {
                name: row.name,
                final_name: row.final_name,
                local: file.path.clone(),
                ticket,
                sent: 0,
                total: file.size,
                state: ItemState::Waiting,
            });
        }
        if items.is_empty() {
            self.phase = UploadPhase::Choosing;
            self.note = Some("The worker granted nothing to upload.".to_string());
            return true;
        }
        self.phase = UploadPhase::Uploading {
            dir: plan.dir,
            items,
            current: None,
        };
        self.note = None;
        true
    }

    /// An ack for one of the panel's requests: a refusal ends the request
    /// with its reason. Returns whether the ack was the panel's.
    pub fn on_ack(&mut self, ack: &Ack) -> bool {
        if !self.mine.contains(&ack.seq) {
            return false;
        }
        let current = matches!(self.phase, UploadPhase::Requested { seq, .. } if seq == ack.seq);
        if current && ack.outcome == AckOutcome::Rejected {
            self.phase = UploadPhase::Choosing;
            self.note = Some(
                ack.detail
                    .clone()
                    .unwrap_or_else(|| "The worker refused the upload.".to_string()),
            );
        }
        true
    }

    /// An error frame naming one of the panel's requests: a refusal.
    pub fn on_error(&mut self, error: &WireError) -> bool {
        let Some(seq) = error.seq else {
            return false;
        };
        self.on_ack(&Ack {
            seq,
            outcome: AckOutcome::Rejected,
            detail: Some(error.message.clone()),
        })
    }

    /// The link dropped: a request still out will not be answered (commands
    /// are not carried across a reconnect). A transfer carries on — its
    /// ticket does not need the socket.
    pub fn link_lost(&mut self) -> bool {
        if matches!(self.phase, UploadPhase::Requested { .. }) {
            self.phase = UploadPhase::Choosing;
            self.note = Some(LINK_LOST.to_string());
            return true;
        }
        false
    }

    // -- the transfer -------------------------------------------------------------

    /// The next file to send, when none is being sent: marks it sending.
    /// `None` when one is under way, or none is left.
    pub fn next_job(&mut self) -> Option<UploadJob> {
        let UploadPhase::Uploading { items, current, .. } = &mut self.phase else {
            return None;
        };
        if current.is_some() {
            return None;
        }
        let index = items.iter().position(|i| i.state == ItemState::Waiting)?;
        let item = &mut items[index];
        item.state = ItemState::Sending;
        *current = Some(index);
        Some(UploadJob {
            index,
            local: item.local.clone(),
            ticket: item.ticket.clone(),
            size: item.total,
        })
    }

    /// `sent` bytes of item `index` are out.
    pub fn progress(&mut self, index: usize, sent: u64) -> bool {
        let UploadPhase::Uploading { items, .. } = &mut self.phase else {
            return false;
        };
        match items.get_mut(index) {
            Some(item) if item.state == ItemState::Sending && item.sent != sent => {
                item.sent = sent.min(item.total);
                true
            }
            _ => false,
        }
    }

    /// Item `index` ended: `Ok(path on the worker)` or `Err(why)`. The last
    /// one ends the upload ([`UploadPhase::Done`]).
    pub fn finished(&mut self, index: usize, result: Result<String, String>) -> bool {
        let UploadPhase::Uploading {
            dir,
            items,
            current,
        } = &mut self.phase
        else {
            return false;
        };
        let Some(item) = items.get_mut(index) else {
            return false;
        };
        if item.state != ItemState::Sending {
            return false;
        }
        item.state = match result {
            Ok(path) => {
                item.sent = item.total;
                ItemState::Written(path)
            }
            Err(why) => ItemState::Failed(why),
        };
        if *current == Some(index) {
            *current = None;
        }
        if items
            .iter()
            .any(|i| matches!(i.state, ItemState::Waiting | ItemState::Sending))
        {
            return true;
        }
        let mut written = Vec::new();
        let mut failed = Vec::new();
        for item in items.iter() {
            match &item.state {
                ItemState::Written(_) => written.push(item.final_name.clone()),
                ItemState::Failed(why) => failed.push((item.final_name.clone(), why.clone())),
                _ => {}
            }
        }
        self.phase = UploadPhase::Done {
            dir: std::mem::take(dir),
            written,
            failed,
        };
        true
    }

    /// Stop. Returns whether a transfer was under way, which the caller then
    /// aborts. What was written stays written.
    pub fn cancel(&mut self) -> bool {
        let sending = matches!(
            &self.phase,
            UploadPhase::Uploading {
                current: Some(_),
                ..
            }
        );
        self.phase = UploadPhase::Cancelled;
        self.note = None;
        sending
    }

    /// Back to an empty list (after Done or Cancelled), same folder.
    pub fn start_over(&mut self) {
        self.files.clear();
        self.phase = UploadPhase::Choosing;
        self.note = None;
    }
}

#[cfg(test)]
mod tests;
