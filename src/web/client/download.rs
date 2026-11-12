//! Downloading files from a worker, as a pure reducer (v9,
//! `remote-control-pz6d`): the Transfer Files panel's Download tab.
//!
//! The person browses the worker's folders (a `browse_dir` with
//! `include_files`), ticks files in the folder on screen, and picks a folder
//! on this machine to save into. Names already there are asked about here,
//! before the worker is: **Replace**, **Keep both** (`name (1).ext`, free on
//! disk and within the batch) or **Cancel**. The worker answers
//! [`begin_download`](super::RemoteClient::begin_download) with a
//! [`DownloadPlan`] — a ticket per file, or a refusal — and the files then
//! come one at a time over [`super::download_http`]. [`DownloadPanel`] holds
//! what the tab shows and folds all of that in:
//!
//! ```text
//! Browsing ──download──▶ Requested(seq) ──plan: tickets──▶ Downloading ──▶ Done
//!    ▲  │                    │                                  │
//!    │  └─names here─▶ Asking ┘ replace / keep both             │
//!    │                  │ cancel          refusal / error       │
//!    └──────────────────┴───────────────────────────────────────┘
//!                       Cancelled ◀── cancel (any phase)
//! ```
//!
//! It sends nothing and touches no file: the front-end (through
//! [`super::session::RemoteSession`]) sends the browse and the request,
//! tells the panel the seqs it got, says which names are taken here, starts
//! the transfer [`DownloadPanel::next_job`] names, and feeds back the
//! listing, the plan, the acks and errors, and each transfer's progress and
//! end. An answer to anything but the latest browse or request is dropped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::host::worker::upload::keep_both_name;
use crate::web::protocol::{
    Ack, AckOutcome, DirEntryView, DirListing, DownloadPlan, FileEntryView, WireError,
};

use super::upload::Resolution;
use super::worker::NOT_SENT;

/// How many of its own seqs the panel remembers, so a superseded request's
/// ack is still recognised as the panel's.
const SEQS_KEPT: usize = 16;

/// Said when Download is asked for with nothing ticked.
pub const NOTHING_SELECTED: &str = "Select at least one file to download.";

/// Said when the destination is not a folder on this machine.
pub const NO_DESTINATION: &str = "Choose a folder on this computer to save into.";

/// Said when the link dropped while the worker was being asked.
pub const LINK_LOST: &str = "The connection to the worker dropped; download again.";

/// Said when a plan granted nothing.
pub const NOTHING_GRANTED: &str = "The worker granted nothing to download.";

/// Said for a file the plan has no ticket for.
pub const NOT_GRANTED: &str = "The worker did not grant this file.";

/// The folder downloads go to by default: this machine's Downloads folder
/// (`~/Downloads`, on every platform) when there is one, else the home
/// folder, else the folder the app runs in.
pub fn default_destination() -> PathBuf {
    let Some(home) = home_dir() else {
        return PathBuf::from(".");
    };
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The folder on this machine `text` names: `~` and `~/…` (or `~\…`) are the
/// home folder, anything else must be absolute. `None` for a blank or
/// relative path.
pub fn resolve_destination(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    if text == "~" {
        return home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return home_dir().map(|home| home.join(rest));
    }
    let path = PathBuf::from(text);
    (!text.is_empty() && path.is_absolute()).then_some(path)
}

/// `path` with the home folder written `~`, as the panel shows a folder.
pub fn tilde(path: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// What to send: a `begin_download`'s arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    /// The worker folder, as listed.
    pub dir: String,
    /// The files' names in it, in list order.
    pub names: Vec<String>,
}

/// Where one file will land here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedFile {
    /// Its name on the worker.
    pub name: String,
    /// The name it gets here.
    pub final_name: String,
    /// Whether it overwrites a file of that name (Replace).
    pub replace: bool,
}

/// How one file of a granted download stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemState {
    /// Not started.
    Waiting,
    /// Its bytes are coming in now.
    Receiving,
    /// Here, at this path.
    Saved(PathBuf),
    /// Not here, with why.
    Failed(String),
}

/// One file of a granted download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadItem {
    /// Its name on the worker.
    pub name: String,
    /// The name it is saved as here (it may change at commit if one
    /// appeared meanwhile; [`ItemState::Saved`] has the final path).
    pub final_name: String,
    /// Whether it overwrites a file of that name.
    pub replace: bool,
    /// Its ticket, when granted.
    pub ticket: Option<String>,
    /// Bytes in so far.
    pub received: u64,
    /// Its size: as granted, then as the worker sent it.
    pub total: u64,
    /// How it stands.
    pub state: ItemState,
}

/// One transfer to start: `GET` `ticket` into `dest` as `final_name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadJob {
    /// Which item, for [`DownloadPanel::progress`] and
    /// [`DownloadPanel::finished`].
    pub index: usize,
    /// Its ticket.
    pub ticket: String,
    /// The folder here.
    pub dest: PathBuf,
    /// Its name on the worker (the base for a keep-both name at commit).
    pub name: String,
    /// The name it is saved as.
    pub final_name: String,
    /// Overwrite a file of that name rather than keep both.
    pub replace: bool,
    /// Its size as granted.
    pub size: u64,
}

/// Where the tab is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadPhase {
    /// Browsing the worker and ticking files.
    Browsing,
    /// Names are already in the destination; the person chooses
    /// ([`Resolution`]).
    Asking {
        /// The destination.
        dest: PathBuf,
        /// The names taken there, in list order.
        clashes: Vec<String>,
    },
    /// A `begin_download` is out as `seq`.
    Requested {
        /// Its seq.
        seq: u64,
        /// The destination.
        dest: PathBuf,
        /// Where each file will land.
        planned: Vec<PlannedFile>,
    },
    /// The files are coming in, one at a time.
    Downloading {
        /// The worker folder, canonical.
        dir: String,
        /// The destination.
        dest: PathBuf,
        /// Every file asked for.
        items: Vec<DownloadItem>,
        /// The one coming in, if any.
        current: Option<usize>,
    },
    /// Every file is saved or has failed.
    Done {
        /// The worker folder, canonical.
        dir: String,
        /// The destination.
        dest: PathBuf,
        /// The files saved: their worker name and where they are here.
        saved: Vec<(String, PathBuf)>,
        /// The ones that were not, with why.
        failed: Vec<(String, String)>,
    },
    /// The person cancelled.
    Cancelled,
}

/// The Download tab's state. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPanel {
    /// The worker folder listed (or being listed first), as the worker
    /// wrote it.
    pub cwd: String,
    /// Its parent, when it has one under `$HOME`.
    pub parent: Option<String>,
    /// The worker's `$HOME`, for `~` and Home.
    pub home: String,
    /// The folder's subfolders.
    pub folders: Vec<DirEntryView>,
    /// The folder's files.
    pub files: Vec<FileEntryView>,
    /// Whether the worker stopped listing early.
    pub truncated: bool,
    /// Whether dot-files and dot-folders are listed.
    pub show_hidden: bool,
    /// Whether a listing is on its way.
    pub loading: bool,
    /// Whether a listing has arrived yet.
    pub listed: bool,
    /// The ticked files' names, in list order.
    pub selected: Vec<String>,
    /// The folder on this machine to save into, as typed.
    pub dest: String,
    /// Where the tab is.
    pub phase: DownloadPhase,
    /// The last thing worth saying: a refusal, an unlistable folder.
    pub note: Option<String>,
    /// The latest browse's seq.
    browse_seq: Option<u64>,
    /// Every seq the panel sent lately, newest last.
    mine: Vec<u64>,
    /// What the next [`DownloadPanel::requested`] asks for.
    pending: Option<(PathBuf, Vec<PlannedFile>)>,
}

impl DownloadPanel {
    /// A panel about to list `start` on the worker (whose `$HOME` is
    /// `home`), saving into `dest`.
    pub fn new(
        start: impl Into<String>,
        home: impl Into<String>,
        dest: impl Into<String>,
    ) -> DownloadPanel {
        DownloadPanel {
            cwd: start.into(),
            parent: None,
            home: home.into(),
            folders: Vec::new(),
            files: Vec::new(),
            truncated: false,
            show_hidden: false,
            loading: false,
            listed: false,
            selected: Vec::new(),
            dest: dest.into(),
            phase: DownloadPhase::Browsing,
            note: None,
            browse_seq: None,
            mine: Vec::new(),
            pending: None,
        }
    }

    /// Whether the person may still browse, tick and pick.
    pub fn is_browsing(&self) -> bool {
        self.phase == DownloadPhase::Browsing
    }

    /// Whether files are coming in (or about to).
    pub fn is_downloading(&self) -> bool {
        matches!(self.phase, DownloadPhase::Downloading { .. })
    }

    /// Whether the worker is being asked or files are coming in: what
    /// switching tabs or closing would cut off.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            DownloadPhase::Requested { .. } | DownloadPhase::Downloading { .. }
        )
    }

    /// Whether `name` is ticked.
    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.iter().any(|n| n == name)
    }

    /// The total size of the ticked files, as listed.
    pub fn selected_bytes(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| self.is_selected(&f.name))
            .fold(0u64, |sum, f| sum.saturating_add(f.size))
    }

    /// `(received, total)` bytes over every file, while downloading.
    pub fn progress_bytes(&self) -> Option<(u64, u64)> {
        let DownloadPhase::Downloading { items, .. } = &self.phase else {
            return None;
        };
        Some(items.iter().fold((0u64, 0u64), |(got, total), item| {
            (
                got.saturating_add(item.received),
                total.saturating_add(item.total),
            )
        }))
    }

    fn remember(&mut self, seq: u64) {
        self.mine.push(seq);
        let excess = self.mine.len().saturating_sub(SEQS_KEPT);
        self.mine.drain(..excess);
    }

    // -- browsing -------------------------------------------------------------

    /// A `browse_dir` went out as `seq` (`None`: it could not be sent).
    pub fn browse_sent(&mut self, seq: Option<u64>) {
        match seq {
            None => {
                self.loading = false;
                self.note = Some(NOT_SENT.to_string());
            }
            Some(seq) => {
                self.remember(seq);
                self.browse_seq = Some(seq);
                self.loading = true;
            }
        }
    }

    /// Whether `seq` is the browse the panel waits on.
    pub fn awaits_listing(&self, seq: u64) -> bool {
        self.loading && self.browse_seq == Some(seq)
    }

    /// A listing. Dropped unless it answers the latest browse. A new folder
    /// clears the ticks; the same folder again keeps the ones still listed.
    pub fn on_listing(&mut self, listing: DirListing) -> bool {
        if !self.awaits_listing(listing.seq) {
            return false;
        }
        self.loading = false;
        if listing.path == self.cwd && self.listed {
            self.selected
                .retain(|name| listing.files.iter().any(|f| &f.name == name));
        } else {
            self.selected.clear();
        }
        self.cwd = listing.path;
        self.parent = listing.parent;
        self.home = listing.home;
        self.folders = listing.entries;
        self.files = listing.files;
        self.truncated = listing.truncated;
        self.listed = true;
        if self.is_browsing() {
            self.note = None;
        }
        true
    }

    /// Tick or untick `name` (a file in the folder on screen).
    pub fn toggle(&mut self, name: &str) -> bool {
        if !self.is_browsing() || !self.files.iter().any(|f| f.name == name) {
            return false;
        }
        if self.is_selected(name) {
            self.selected.retain(|n| n != name);
        } else {
            // Kept in list order, so the request asks in the order shown.
            self.selected.push(name.to_string());
            let order: Vec<&str> = self.files.iter().map(|f| f.name.as_str()).collect();
            self.selected
                .sort_by_key(|n| order.iter().position(|o| *o == n));
        }
        self.note = None;
        true
    }

    /// Tick every file in the folder — or, all ticked already, none.
    pub fn select_all(&mut self) -> bool {
        if !self.is_browsing() || self.files.is_empty() {
            return false;
        }
        if self.selected.len() == self.files.len() {
            self.selected.clear();
        } else {
            self.selected = self.files.iter().map(|f| f.name.clone()).collect();
        }
        self.note = None;
        true
    }

    /// Save into `dest` (as typed) instead.
    pub fn set_dest(&mut self, dest: impl Into<String>) -> bool {
        if !self.is_browsing() {
            return false;
        }
        self.dest = dest.into();
        true
    }

    // -- asking ---------------------------------------------------------------

    /// What to send for Download, saving into `dest` (`None`: the typed
    /// destination is not a folder here). `exists` says whether a name is
    /// taken there. Names that are go to [`DownloadPhase::Asking`] first,
    /// and nothing is sent yet. `None` (with the note saying why, unless
    /// asking) when there is nothing to send.
    pub fn request(
        &mut self,
        dest: Option<PathBuf>,
        exists: impl Fn(&str) -> bool,
    ) -> Option<DownloadRequest> {
        if !self.is_browsing() || !self.listed {
            return None;
        }
        if self.selected.is_empty() {
            self.note = Some(NOTHING_SELECTED.to_string());
            return None;
        }
        let Some(dest) = dest else {
            self.note = Some(NO_DESTINATION.to_string());
            return None;
        };
        let clashes: Vec<String> = self
            .selected
            .iter()
            .filter(|n| exists(n))
            .cloned()
            .collect();
        if !clashes.is_empty() {
            self.phase = DownloadPhase::Asking { dest, clashes };
            self.note = None;
            return None;
        }
        let planned = self
            .selected
            .iter()
            .map(|name| PlannedFile {
                name: name.clone(),
                final_name: name.clone(),
                replace: false,
            })
            .collect();
        Some(self.ask(dest, planned))
    }

    fn ask(&mut self, dest: PathBuf, planned: Vec<PlannedFile>) -> DownloadRequest {
        let names = planned.iter().map(|p| p.name.clone()).collect();
        self.pending = Some((dest, planned));
        DownloadRequest {
            dir: self.cwd.clone(),
            names,
        }
    }

    /// The person's answer to names already here: a request replacing them
    /// or keeping both, or (`Cancel`) back to browsing. `exists` is asked
    /// again: what is in the folder now decides the keep-both names.
    pub fn resolve(
        &mut self,
        resolution: Resolution,
        exists: impl Fn(&str) -> bool,
    ) -> Option<DownloadRequest> {
        let DownloadPhase::Asking { dest, .. } = &self.phase else {
            return None;
        };
        let dest = dest.clone();
        let planned = match resolution {
            Resolution::Cancel => {
                self.phase = DownloadPhase::Browsing;
                self.note = None;
                return None;
            }
            Resolution::Replace => self
                .selected
                .iter()
                .map(|name| PlannedFile {
                    name: name.clone(),
                    final_name: name.clone(),
                    replace: exists(name),
                })
                .collect(),
            Resolution::KeepBoth => keep_both_names(&self.selected, &exists),
        };
        Some(self.ask(dest, planned))
    }

    /// The request went out as `seq` (`None`: it could not be sent).
    pub fn requested(&mut self, seq: Option<u64>) {
        let pending = self.pending.take();
        match (seq, pending) {
            (Some(seq), Some((dest, planned))) => {
                self.remember(seq);
                self.phase = DownloadPhase::Requested { seq, dest, planned };
                self.note = None;
            }
            _ => {
                self.phase = DownloadPhase::Browsing;
                self.note = Some(NOT_SENT.to_string());
            }
        }
    }

    // -- the worker's answers ---------------------------------------------------

    /// The worker's plan. Dropped unless it answers the request out now.
    pub fn on_plan(&mut self, plan: DownloadPlan) -> bool {
        let DownloadPhase::Requested { seq, dest, planned } = &self.phase else {
            return false;
        };
        if plan.seq != *seq {
            return false;
        }
        if let Some(refusal) = plan.refusal {
            self.phase = DownloadPhase::Browsing;
            self.note = Some(refusal);
            return true;
        }
        let dest = dest.clone();
        let mut items = Vec::with_capacity(planned.len());
        for want in planned {
            let row = plan.files.iter().find(|f| f.name == want.name);
            let ticket = row.and_then(|r| r.ticket.clone());
            items.push(DownloadItem {
                name: want.name.clone(),
                final_name: want.final_name.clone(),
                replace: want.replace,
                state: if ticket.is_some() {
                    ItemState::Waiting
                } else {
                    ItemState::Failed(NOT_GRANTED.to_string())
                },
                ticket,
                received: 0,
                total: row.map_or(0, |r| r.size),
            });
        }
        if !items.iter().any(|i| i.ticket.is_some()) {
            self.phase = DownloadPhase::Browsing;
            self.note = Some(NOTHING_GRANTED.to_string());
            return true;
        }
        self.phase = DownloadPhase::Downloading {
            dir: plan.dir,
            dest,
            items,
            current: None,
        };
        self.note = None;
        true
    }

    /// An ack for one of the panel's seqs: a refused browse is the note, a
    /// refused request ends it with its reason. Returns whether the ack was
    /// the panel's.
    pub fn on_ack(&mut self, ack: &Ack) -> bool {
        if !self.mine.contains(&ack.seq) {
            return false;
        }
        if ack.outcome != AckOutcome::Rejected {
            return true;
        }
        if self.browse_seq == Some(ack.seq) && self.loading {
            self.loading = false;
            self.note = Some(
                ack.detail
                    .clone()
                    .unwrap_or_else(|| "The worker could not list that folder.".to_string()),
            );
        }
        if matches!(self.phase, DownloadPhase::Requested { seq, .. } if seq == ack.seq) {
            self.phase = DownloadPhase::Browsing;
            self.note = Some(
                ack.detail
                    .clone()
                    .unwrap_or_else(|| "The worker refused the download.".to_string()),
            );
        }
        true
    }

    /// An error frame naming one of the panel's seqs: a refusal.
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

    /// The link dropped: a browse or request still out will not be answered
    /// (commands are not carried across a reconnect). A transfer carries on
    /// — its ticket does not need the socket.
    pub fn link_lost(&mut self) -> bool {
        let mut changed = false;
        if self.loading {
            self.loading = false;
            changed = true;
        }
        if matches!(self.phase, DownloadPhase::Requested { .. }) {
            self.phase = DownloadPhase::Browsing;
            self.note = Some(LINK_LOST.to_string());
            changed = true;
        }
        changed
    }

    // -- the transfer -------------------------------------------------------------

    /// The next file to fetch, when none is coming in: marks it receiving.
    /// `None` when one is under way, or none is left.
    pub fn next_job(&mut self) -> Option<DownloadJob> {
        let DownloadPhase::Downloading {
            dest,
            items,
            current,
            ..
        } = &mut self.phase
        else {
            return None;
        };
        if current.is_some() {
            return None;
        }
        let index = items.iter().position(|i| i.state == ItemState::Waiting)?;
        let item = &mut items[index];
        item.state = ItemState::Receiving;
        *current = Some(index);
        Some(DownloadJob {
            index,
            ticket: item.ticket.clone().unwrap_or_default(),
            dest: dest.clone(),
            name: item.name.clone(),
            final_name: item.final_name.clone(),
            replace: item.replace,
            size: item.total,
        })
    }

    /// `received` of `total` bytes of item `index` are in (`total` is what
    /// the worker sent as its length, which wins over the plan's).
    pub fn progress(&mut self, index: usize, received: u64, total: u64) -> bool {
        let DownloadPhase::Downloading { items, .. } = &mut self.phase else {
            return false;
        };
        match items.get_mut(index) {
            Some(item)
                if item.state == ItemState::Receiving
                    && (item.received != received || item.total != total) =>
            {
                item.total = total;
                item.received = received.min(total);
                true
            }
            _ => false,
        }
    }

    /// Item `index` ended: `Ok(where it is here)` or `Err(why)`. A failure
    /// leaves the rest going; the last one ends the download
    /// ([`DownloadPhase::Done`]).
    pub fn finished(&mut self, index: usize, result: Result<PathBuf, String>) -> bool {
        let DownloadPhase::Downloading {
            dir,
            dest,
            items,
            current,
        } = &mut self.phase
        else {
            return false;
        };
        let Some(item) = items.get_mut(index) else {
            return false;
        };
        if item.state != ItemState::Receiving {
            return false;
        }
        item.state = match result {
            Ok(path) => {
                item.received = item.total;
                ItemState::Saved(path)
            }
            Err(why) => ItemState::Failed(why),
        };
        if *current == Some(index) {
            *current = None;
        }
        if items
            .iter()
            .any(|i| matches!(i.state, ItemState::Waiting | ItemState::Receiving))
        {
            return true;
        }
        let mut saved = Vec::new();
        let mut failed = Vec::new();
        for item in items.iter() {
            match &item.state {
                ItemState::Saved(path) => saved.push((item.name.clone(), path.clone())),
                ItemState::Failed(why) => failed.push((item.name.clone(), why.clone())),
                _ => {}
            }
        }
        self.phase = DownloadPhase::Done {
            dir: std::mem::take(dir),
            dest: std::mem::take(dest),
            saved,
            failed,
        };
        true
    }

    /// Stop. Returns whether a transfer was under way, which the caller then
    /// aborts. What was saved stays saved; the file coming in is not.
    pub fn cancel(&mut self) -> bool {
        let receiving = matches!(
            &self.phase,
            DownloadPhase::Downloading {
                current: Some(_),
                ..
            }
        );
        self.phase = DownloadPhase::Cancelled;
        self.pending = None;
        self.note = None;
        receiving
    }

    /// Back to browsing (after Done or Cancelled): the same folders, nothing
    /// ticked.
    pub fn start_over(&mut self) {
        self.selected.clear();
        self.phase = DownloadPhase::Browsing;
        self.pending = None;
        self.note = None;
    }
}

/// The keep-both names for `names`: a name free here keeps it, a taken one
/// gets the first `stem (n).ext` free both here and within the batch. Free
/// names are reserved first, so a generated name never lands on one another
/// file of the batch keeps.
fn keep_both_names(names: &[String], exists: &impl Fn(&str) -> bool) -> Vec<PlannedFile> {
    let mut used: HashSet<String> = names.iter().cloned().collect();
    names
        .iter()
        .map(|name| {
            if !exists(name) {
                return PlannedFile {
                    name: name.clone(),
                    final_name: name.clone(),
                    replace: false,
                };
            }
            let final_name = (1..)
                .map(|n| keep_both_name(name, n))
                .find(|candidate| !used.contains(candidate) && !exists(candidate))
                .unwrap_or_else(|| name.clone());
            used.insert(final_name.clone());
            PlannedFile {
                name: name.clone(),
                final_name,
                replace: false,
            }
        })
        .collect()
}

/// Whether `name` is taken in `dir` on this machine (a file, a folder, a
/// dangling link — anything).
pub fn taken_here(dir: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(dir.join(name)).is_ok()
}

#[cfg(test)]
mod tests;
