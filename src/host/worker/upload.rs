//! Uploading files to a worker (v8, `remote-control-usem`): deciding what to
//! grant.
//!
//! A window asks to put some files into one folder
//! ([`crate::web::protocol::worker_command::BEGIN_UPLOAD`]). Before any ticket
//! exists this checks, in order:
//!
//! 1. **How many** — 1 to [`MAX_FILES`].
//! 2. **The names** — each one path component ([`validate_name`]), none twice.
//! 3. **The folder** — an existing folder inside `$HOME`, with the same
//!    confinement and wording as opening a project ([`projects::validate_open`]).
//! 4. **The space** — the total must fit in the folder's free space with
//!    [`DISK_MARGIN_BYTES`] to spare.
//! 5. **The names already taken** — with `ask`, any taken name answers with
//!    the conflicts and grants nothing; with `replace` a taken name is
//!    overwritten (unless it is a folder); with `keep_both` it gets the first
//!    free `stem (n).ext` ([`keep_both_name`]).
//!
//! [`plan_names`] — step 5 — is pure over an injected lookup, so the naming
//! rules are tested without a filesystem; [`plan`] runs every step against the
//! [`FileSystem`] seam.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::contracts::FileSystem;
use crate::web::protocol::{OnConflict, UploadFile};
use crate::web::upload::size_label;

use super::projects::{self, Confinement};

/// The most files one request may upload.
pub const MAX_FILES: usize = 256;

/// The longest file name, in bytes (what Linux and macOS file systems allow).
pub const MAX_NAME_BYTES: usize = 255;

/// The free space an upload must leave behind: 256 MiB, flat. A worker whose
/// disk fills up stops its agents mid-write and cannot even log why; the
/// health probe reports free bytes but not the disk's size, so a percentage is
/// not available, and 256 MiB is room for logs, git objects and a build's
/// scratch while still letting a nearly full small disk take a small file.
pub const DISK_MARGIN_BYTES: u64 = 256 * 1024 * 1024;

/// What is at `dir/name` right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// Nothing: the name is free.
    Free,
    /// A file (or anything that is not a folder).
    File,
    /// A folder, which no upload may replace.
    Folder,
}

/// One file's place in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedFile {
    /// The name asked for.
    pub name: String,
    /// The name it gets.
    pub final_name: String,
    /// The size asked for.
    pub size: u64,
    /// Whether the asked name was taken.
    pub exists: bool,
    /// Whether the write may overwrite `final_name`.
    pub replace: bool,
}

/// What the worker answers a valid request with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Grant every file.
    Ready {
        /// The canonical target folder.
        dir: PathBuf,
        files: Vec<PlannedFile>,
    },
    /// `ask`, and some names are taken (`exists`): grant nothing.
    Conflicts {
        /// The canonical target folder.
        dir: PathBuf,
        files: Vec<PlannedFile>,
    },
}

/// Whether `name` may be a file name on the worker: one path component, so
/// no `/`, no `\` (a Windows client's separator), no NUL or other control
/// character, not empty, not `.` or `..`, and at most [`MAX_NAME_BYTES`].
pub fn validate_name(name: &str) -> Result<(), String> {
    let shown = || format!("\"{}\"", name.escape_debug());
    if name.is_empty() {
        return Err("A file to upload has no name.".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("{} cannot be a file name.", shown()));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "{} is a path, not a file name: upload files, not folders.",
            shown()
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(format!("{} has a control character in its name.", shown()));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "\"{}…\" is too long a file name (at most {MAX_NAME_BYTES} bytes).",
            name.chars().take(40).collect::<String>()
        ));
    }
    Ok(())
}

/// `name` with ` (n)` before its extension: `photo.png` → `photo (1).png`,
/// `Makefile` → `Makefile (1)`, a dotfile `.env` → `.env (1)`. Only the last
/// extension counts (`a.tar.gz` → `a.tar (1).gz`).
pub fn keep_both_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => {
            format!("{} ({n}){}", &name[..dot], &name[dot..])
        }
        _ => format!("{name} ({n})"),
    }
}

/// Step 5: the final names, given what is in the folder now (`lookup`).
///
/// Free names keep their name, and are reserved first, so a generated
/// `x (1).txt` never lands on a name another file of the batch asked for.
/// Under `keep_both` every taken name gets the first free `stem (n).ext`, `n`
/// from 1, free both in the folder and in the batch.
pub fn plan_names(
    dir: PathBuf,
    files: &[UploadFile],
    on_conflict: OnConflict,
    lookup: impl Fn(&str) -> Taken,
) -> Result<Plan, String> {
    let taken: Vec<Taken> = files.iter().map(|f| lookup(&f.name)).collect();
    let mut planned: Vec<PlannedFile> = files
        .iter()
        .zip(&taken)
        .map(|(file, taken)| PlannedFile {
            name: file.name.clone(),
            final_name: file.name.clone(),
            size: file.size,
            exists: *taken != Taken::Free,
            replace: false,
        })
        .collect();
    if !planned.iter().any(|f| f.exists) {
        return Ok(Plan::Ready {
            dir,
            files: planned,
        });
    }
    match on_conflict {
        OnConflict::Ask => Ok(Plan::Conflicts {
            dir,
            files: planned,
        }),
        OnConflict::Replace => {
            if let Some((file, _)) = planned
                .iter()
                .zip(&taken)
                .find(|(_, taken)| **taken == Taken::Folder)
            {
                return Err(format!(
                    "{} is a folder on the worker, so it cannot be replaced.",
                    file.name
                ));
            }
            for file in &mut planned {
                file.replace = true;
            }
            Ok(Plan::Ready {
                dir,
                files: planned,
            })
        }
        OnConflict::KeepBoth => {
            let mut used: HashSet<String> = planned
                .iter()
                .filter(|f| !f.exists)
                .map(|f| f.name.clone())
                .collect();
            for file in planned.iter_mut().filter(|f| f.exists) {
                let mut n = 1u32;
                let name = loop {
                    let candidate = keep_both_name(&file.name, n);
                    if !used.contains(&candidate) && lookup(&candidate) == Taken::Free {
                        break candidate;
                    }
                    n = n.checked_add(1).ok_or_else(|| {
                        format!("No free name is left for {} on the worker.", file.name)
                    })?;
                };
                if name.len() > MAX_NAME_BYTES {
                    return Err(format!(
                        "{} is taken, and its numbered copy's name would be too long.",
                        file.name
                    ));
                }
                used.insert(name.clone());
                file.final_name = name;
            }
            Ok(Plan::Ready {
                dir,
                files: planned,
            })
        }
    }
}

/// Steps 1–4: refuse a request that cannot be granted in any case.
pub fn check_request(files: &[UploadFile]) -> Result<(), String> {
    if files.is_empty() {
        return Err("Choose at least one file to upload.".to_string());
    }
    if files.len() > MAX_FILES {
        return Err(format!(
            "That is {} files; upload at most {MAX_FILES} at a time.",
            files.len()
        ));
    }
    let mut seen = HashSet::new();
    for file in files {
        validate_name(&file.name)?;
        if !seen.insert(file.name.as_str()) {
            return Err(format!(
                "{} is in the list twice: one file per name.",
                file.name
            ));
        }
    }
    Ok(())
}

/// Whether `total` bytes fit in `free` with [`DISK_MARGIN_BYTES`] to spare.
/// An unknown free space (`None`) is not a refusal: the write fails honestly
/// if the disk does fill.
pub fn check_space(total: u64, free: Option<u64>) -> Result<(), String> {
    let Some(free) = free else {
        return Ok(());
    };
    if total.saturating_add(DISK_MARGIN_BYTES) <= free {
        return Ok(());
    }
    Err(format!(
        "Not enough free space on the worker: the files need {}, and {} is free \
         (FlightDeck keeps {} spare).",
        size_label(total),
        size_label(free),
        size_label(DISK_MARGIN_BYTES)
    ))
}

/// Every step, against the worker's filesystem: `disk_free` answers for the
/// canonical folder.
pub fn plan(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &str,
    files: &[UploadFile],
    on_conflict: OnConflict,
    disk_free: impl FnOnce(&Path) -> Option<u64>,
) -> Result<Plan, String> {
    check_request(files)?;
    let canonical = projects::validate_open(fs, conf, dir).map_err(|e| e.to_string())?;
    let total = files.iter().fold(0u64, |sum, f| sum.saturating_add(f.size));
    check_space(total, disk_free(&canonical))?;
    let lookup = |name: &str| {
        let path = canonical.join(name);
        if fs.is_dir(&path) {
            Taken::Folder
        } else if fs.exists(&path) {
            Taken::File
        } else {
            Taken::Free
        }
    };
    plan_names(canonical.clone(), files, on_conflict, lookup)
}

/// `path` as a person reads it on the worker: `~/proj/assets` under `home`,
/// else as it is.
pub fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

// The worker is Linux and macOS only (W2); its tests use unix paths.
#[cfg(all(test, unix))]
mod tests;
