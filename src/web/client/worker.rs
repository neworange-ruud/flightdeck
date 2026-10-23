//! A worker's project picker, as a pure reducer (v7, `specs/WORKER_PRD.md`
//! W9, W13, W16).
//!
//! A worker has no screen of its own, so opening a project on it happens from
//! the remote window: scan the projects root for repositories, browse `$HOME`
//! folder by folder, or clone a URL. The worker answers each with its own
//! frame ([`DirListing`], [`ProjectScan`], [`CloneProgress`], [`CloneResult`])
//! beside the [`Ack`] for the command, and refuses anything outside `$HOME`
//! in words.
//!
//! [`ProjectPicker`] holds what the picker shows and folds those answers in.
//! It sends nothing itself: the front-end sends through
//! [`super::RemoteClient`] (`browse_dir`, `scan_projects`, `open_project_path`,
//! `clone_project`), tells the picker the seq it got back, and feeds it every
//! answer. Answers to a request that has since been superseded are dropped, so
//! a slow listing never repaints over the folder the person has moved on to.

use crate::web::protocol::{
    Ack, AckOutcome, CloneProgress, CloneResult, DirEntryView, DirListing, ProjectId, ProjectScan,
    ScannedRepo, WireError,
};

/// One thing a worker said about a running clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneEvent {
    /// How far it has got.
    Progress(CloneProgress),
    /// How it ended.
    Result(CloneResult),
}

/// Said when a request could not be sent at all.
pub const NOT_SENT: &str = "Not connected to the worker — nothing was sent.";

/// A clone the picker started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneState {
    /// The worker's id for it: the seq of the `clone_project` command.
    pub clone_id: u64,
    /// The URL being cloned.
    pub url: String,
    /// What git says it is doing.
    pub phase: String,
    /// How far through that phase, when git says.
    pub percent: Option<u8>,
    /// `Ok(path)` once cloned (and opened), `Err(git's words)` once failed.
    pub done: Option<Result<String, String>>,
}

/// The last project scan, as the picker shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scanned {
    /// The root scanned.
    pub root: String,
    /// Whether it exists at all.
    pub root_exists: bool,
    /// The repositories found.
    pub repos: Vec<ScannedRepo>,
}

/// How many of its own seqs the picker remembers, so the ack that trails an
/// answer (or a superseded request) is still recognised as the picker's.
const SEQS_KEPT: usize = 32;

/// The requests the picker is waiting on, by seq.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Pending {
    /// The latest browse, and whether its listing is still to come.
    browse: Option<(u64, bool)>,
    /// The latest scan, and whether its answer is still to come.
    scan: Option<(u64, bool)>,
    open: Option<(u64, String)>,
    /// Every seq the picker sent lately, newest last.
    mine: Vec<u64>,
}

impl Pending {
    fn waiting(slot: Option<(u64, bool)>) -> bool {
        slot.is_some_and(|(_, waiting)| waiting)
    }

    fn latest(slot: Option<(u64, bool)>, seq: u64) -> bool {
        slot.is_some_and(|(latest, _)| latest == seq)
    }

    fn remember(&mut self, seq: u64) {
        self.mine.push(seq);
        let excess = self.mine.len().saturating_sub(SEQS_KEPT);
        self.mine.drain(..excess);
    }
}

/// The project picker's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectPicker {
    /// `$HOME` on the worker: where browsing starts and the highest it goes.
    pub home: String,
    /// The folder being browsed, once a listing arrived.
    pub cwd: Option<String>,
    /// Its parent, or `None` at `$HOME`.
    pub parent: Option<String>,
    /// Its subfolders.
    pub entries: Vec<DirEntryView>,
    /// The worker stopped listing before the end.
    pub truncated: bool,
    /// Whether hidden folders are listed.
    pub show_hidden: bool,
    /// The last scan of the projects root.
    pub scanned: Option<Scanned>,
    /// The clone URL field.
    pub clone_url: String,
    /// The optional folder-name field.
    pub clone_name: String,
    /// The clone this picker started, if any.
    pub clone: Option<CloneState>,
    /// The last refusal or failure, verbatim, for the picker's error line.
    pub error: Option<String>,
    /// The last success worth saying (`Opened project 'x'.`).
    pub status: Option<String>,
    /// The project the worker opened for this picker — by an open or a
    /// successful clone. A front-end closes the picker when it sees one.
    pub opened: Option<OpenedProject>,
    pending: Pending,
}

/// A project the worker opened at this picker's request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedProject {
    /// Where it is on the worker.
    pub path: String,
    /// Its id, when the worker said (a clone does).
    pub project_id: Option<ProjectId>,
}

impl ProjectPicker {
    /// A picker for a worker whose `$HOME` is `home`.
    pub fn new(home: impl Into<String>) -> ProjectPicker {
        ProjectPicker {
            home: home.into(),
            ..ProjectPicker::default()
        }
    }

    /// Whether any request is still unanswered.
    pub fn is_busy(&self) -> bool {
        Pending::waiting(self.pending.browse)
            || Pending::waiting(self.pending.scan)
            || self.pending.open.is_some()
            || self.clone.as_ref().is_some_and(|c| c.done.is_none())
    }

    fn sent(&mut self, seq: Option<u64>) -> Option<u64> {
        match seq {
            None => self.error = Some(NOT_SENT.to_string()),
            Some(seq) => {
                self.pending.remember(seq);
                self.error = None;
                self.status = None;
            }
        }
        seq
    }

    // -- requests sent ------------------------------------------------------

    /// A `browse_dir` went out as `seq` (`None`: it could not be sent).
    pub fn browse_sent(&mut self, seq: Option<u64>) {
        self.pending.browse = self.sent(seq).map(|seq| (seq, true));
    }

    /// A `scan_projects` went out as `seq`.
    pub fn scan_sent(&mut self, seq: Option<u64>) {
        self.pending.scan = self.sent(seq).map(|seq| (seq, true));
    }

    /// An `open_project_path` for `path` went out as `seq`.
    pub fn open_sent(&mut self, seq: Option<u64>, path: &str) {
        self.pending.open = self.sent(seq).map(|seq| (seq, path.to_string()));
    }

    /// A `clone_project` for `url` went out as `seq`, which is its clone id.
    pub fn clone_sent(&mut self, seq: Option<u64>, url: &str) {
        self.clone = self.sent(seq).map(|clone_id| CloneState {
            clone_id,
            url: url.to_string(),
            phase: "Starting".to_string(),
            percent: None,
            done: None,
        });
    }

    // -- answers ------------------------------------------------------------

    /// A directory listing. Dropped unless it answers the latest browse.
    pub fn on_listing(&mut self, listing: DirListing) -> bool {
        if self.pending.browse != Some((listing.seq, true)) {
            return false;
        }
        self.pending.browse = Some((listing.seq, false));
        self.home = listing.home;
        self.cwd = Some(listing.path);
        self.parent = listing.parent;
        self.entries = listing.entries;
        self.truncated = listing.truncated;
        true
    }

    /// A project scan. Dropped unless it answers the latest scan; a refused
    /// scan is the error line.
    pub fn on_scan(&mut self, scan: ProjectScan) -> bool {
        if self.pending.scan != Some((scan.seq, true)) {
            return false;
        }
        self.pending.scan = Some((scan.seq, false));
        if let Some(refusal) = scan.refusal {
            self.error = Some(refusal);
        }
        self.scanned = Some(Scanned {
            root: scan.root,
            root_exists: scan.root_exists,
            repos: scan.repos,
        });
        true
    }

    /// Progress of this picker's clone; any other clone's is dropped.
    pub fn on_clone_progress(&mut self, progress: CloneProgress) -> bool {
        match self.clone.as_mut() {
            Some(clone) if clone.clone_id == progress.clone_id && clone.done.is_none() => {
                clone.phase = progress.phase;
                clone.percent = progress.percent;
                true
            }
            _ => false,
        }
    }

    /// How this picker's clone ended. On success the worker has opened it.
    pub fn on_clone_result(&mut self, result: CloneResult) -> bool {
        let Some(clone) = self.clone.as_mut() else {
            return false;
        };
        if clone.clone_id != result.clone_id || clone.done.is_some() {
            return false;
        }
        if result.ok {
            let path = result.path.unwrap_or_default();
            clone.done = Some(Ok(path.clone()));
            self.opened = Some(OpenedProject {
                path,
                project_id: result.project_id,
            });
        } else {
            let why = result
                .error
                .unwrap_or_else(|| "The clone failed.".to_string());
            clone.done = Some(Err(why.clone()));
            self.error = Some(why);
        }
        true
    }

    /// A clone event, whichever it is.
    pub fn on_clone_event(&mut self, event: CloneEvent) -> bool {
        match event {
            CloneEvent::Progress(progress) => self.on_clone_progress(progress),
            CloneEvent::Result(result) => self.on_clone_result(result),
        }
    }

    /// An ack for one of this picker's requests. A refusal becomes the error
    /// line and ends the request; an applied open is the project opened.
    /// Returns whether the ack was the picker's.
    pub fn on_ack(&mut self, ack: &Ack) -> bool {
        let refused = ack.outcome == AckOutcome::Rejected;
        let why = || {
            ack.detail
                .clone()
                .unwrap_or_else(|| "The worker refused that.".to_string())
        };
        if Pending::latest(self.pending.browse, ack.seq) {
            // The listing itself is the answer; only a refusal ends it here.
            if refused {
                self.pending.browse = None;
                self.error = Some(why());
            }
            return true;
        }
        if Pending::latest(self.pending.scan, ack.seq) {
            if refused {
                self.pending.scan = None;
                self.error = Some(why());
            }
            return true;
        }
        if let Some((seq, path)) = self.pending.open.clone() {
            if seq == ack.seq {
                self.pending.open = None;
                if refused {
                    self.error = Some(why());
                } else {
                    self.status = ack.detail.clone();
                    self.opened = Some(OpenedProject {
                        path,
                        project_id: None,
                    });
                }
                return true;
            }
        }
        if let Some(clone) = self.clone.as_mut() {
            if clone.clone_id == ack.seq {
                if refused {
                    let why = why();
                    clone.done = Some(Err(why.clone()));
                    self.error = Some(why);
                } else if let Some(detail) = &ack.detail {
                    self.status = Some(detail.clone());
                }
                return true;
            }
        }
        // A superseded request's ack: the picker's, and nothing to say.
        self.pending.mine.contains(&ack.seq)
    }

    /// An error frame naming one of this picker's requests (a malformed
    /// argument, say). Treated as a refusal.
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
}

#[cfg(test)]
mod tests;
