//! Downloading files from a worker (v9, `remote-control-pz6d`): deciding what
//! to grant.
//!
//! A window asks for some files from one folder
//! ([`crate::web::protocol::worker_command::BEGIN_DOWNLOAD`]). Before any
//! ticket exists this checks, in order:
//!
//! 1. **How many** — 1 to [`MAX_FILES`].
//! 2. **The names** — each one path component (the same rule as an upload's,
//!    [`upload::validate_name`], worded for downloads), none twice.
//! 3. **The folder** — an existing folder inside `$HOME`, with the same
//!    confinement and wording as opening a project ([`projects::validate_open`]).
//! 4. **Each file** — `dir/name` exists and resolves (through any link) to a
//!    regular file inside `$HOME`.
//!
//! All or nothing: the first name that fails refuses the whole request, in a
//! sentence naming it. [`plan`] is pure over the [`FileSystem`] seam; the
//! route that serves the bytes checks the file again when it is fetched.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::contracts::FileSystem;

use super::projects::{self, Confinement};
use super::upload;

/// The most files one request may download (as many as one upload).
pub const MAX_FILES: usize = upload::MAX_FILES;

/// One file the worker will hand out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedFile {
    /// The name asked for: the name the download suggests.
    pub name: String,
    /// The file it resolves to, canonical, inside `$HOME`.
    pub path: PathBuf,
    /// Its size now.
    pub size: u64,
}

/// A request the worker can grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The canonical source folder.
    pub dir: PathBuf,
    /// One per name, in the order asked.
    pub files: Vec<PlannedFile>,
}

/// Steps 1–2: refuse a request that cannot be granted in any case.
pub fn check_request(names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("Choose at least one file to download.".to_string());
    }
    if names.len() > MAX_FILES {
        return Err(format!(
            "That is {} files; download at most {MAX_FILES} at a time.",
            names.len()
        ));
    }
    let mut seen = HashSet::new();
    for name in names {
        upload::validate_name_to(name, "download")?;
        if !seen.insert(name.as_str()) {
            return Err(format!("{name} is in the list twice: one file per name."));
        }
    }
    Ok(())
}

/// Step 4 for one name in canonical `dir`: the canonical file and its size.
fn check_file(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &std::path::Path,
    name: &str,
) -> Result<PlannedFile, String> {
    let gone = || format!("{name} is no longer there.");
    let canonical = fs.canonicalize(&dir.join(name)).map_err(|_| gone())?;
    if !canonical.starts_with(conf.home()) {
        return Err(format!("{name} points outside your home folder."));
    }
    if fs.is_dir(&canonical) {
        return Err(format!("{name} is a folder; download files, not folders."));
    }
    if !fs.is_file(&canonical) {
        return Err(format!(
            "{name} is not a regular file, so it cannot be downloaded."
        ));
    }
    let size = fs.file_len(&canonical).map_err(|_| gone())?;
    Ok(PlannedFile {
        name: name.to_string(),
        path: canonical,
        size,
    })
}

/// Every step, against the worker's filesystem.
pub fn plan(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &str,
    names: &[String],
) -> Result<Planned, String> {
    check_request(names)?;
    let canonical = projects::validate_open(fs, conf, dir).map_err(|e| e.to_string())?;
    let files = names
        .iter()
        .map(|name| check_file(fs, conf, &canonical, name))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Planned {
        dir: canonical,
        files,
    })
}

// The worker is Linux and macOS only (W2); its tests use unix paths.
#[cfg(all(test, unix))]
mod tests;
