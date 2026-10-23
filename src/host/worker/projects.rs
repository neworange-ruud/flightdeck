//! Opening projects on a worker (`specs/WORKER_PRD.md` §4.4, W9/W16):
//! browse from `$HOME` down, scan `projects_root`, open a path.
//!
//! Everything is confined to `$HOME`: a requested path is canonicalised and
//! refused when it resolves outside it, including through a symlink.

// stub: filled by E

use std::path::{Path, PathBuf};

use crate::contracts::{FileSystem, Result};

/// The most entries one listing returns; more sets `truncated`.
pub const MAX_ENTRIES: usize = 2000;

/// The confinement root: the account's home directory, canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confinement {
    home: PathBuf,
}

/// Why a project operation was refused. `Display` is a sentence for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectOpError {
    OutsideHome(String),
    NotFound(String),
    NotADirectory(String),
    NotARepository(String),
    RootOutsideHome(String),
    Io(String),
}

impl std::fmt::Display for ProjectOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // stub: filled by E (final wording)
        match self {
            ProjectOpError::OutsideHome(p) => write!(f, "{p} is outside your home folder."),
            ProjectOpError::NotFound(p) => write!(f, "{p} does not exist."),
            ProjectOpError::NotADirectory(p) => write!(f, "{p} is not a folder."),
            ProjectOpError::NotARepository(p) => write!(f, "{p} is not a Git repository."),
            ProjectOpError::RootOutsideHome(p) => {
                write!(f, "The projects root {p} is outside your home folder.")
            }
            ProjectOpError::Io(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ProjectOpError {}

impl Confinement {
    /// Confine to `home`, canonicalised.
    pub fn new(_fs: &dyn FileSystem, home: &Path) -> Result<Confinement> {
        // stub: filled by E (canonicalize)
        Ok(Confinement {
            home: home.to_path_buf(),
        })
    }

    /// The canonical home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Resolve a requested path: none, `""` or `~` is home; `~/x` and a
    /// relative `x` are under home; the canonical result must be inside home.
    pub fn resolve(
        &self,
        _fs: &dyn FileSystem,
        _requested: Option<&str>,
    ) -> std::result::Result<PathBuf, ProjectOpError> {
        // stub: filled by E
        Err(not_implemented())
    }
}

/// One folder of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_git_repo: bool,
}

/// The folders inside one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub path: PathBuf,
    /// `None` at home: browsing never goes above it.
    pub parent: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

fn not_implemented() -> ProjectOpError {
    ProjectOpError::Io("project browsing is not implemented in this build yet".to_string())
}

/// List the folders in `requested` (dirs only, hidden ones only when
/// `show_hidden`, sorted case-insensitively).
pub fn browse(
    _fs: &dyn FileSystem,
    _conf: &Confinement,
    _requested: Option<&str>,
    _show_hidden: bool,
) -> std::result::Result<Listing, ProjectOpError> {
    // stub: filled by E
    Err(not_implemented())
}

/// The repositories under `root`, `depth` levels deep (a repository is not
/// descended into; a missing root is empty).
pub fn scan(
    _fs: &dyn FileSystem,
    _conf: &Confinement,
    _root: &Path,
    _depth: usize,
) -> std::result::Result<Vec<Entry>, ProjectOpError> {
    // stub: filled by E
    Ok(Vec::new())
}

/// Check that `requested` may be opened as a project: inside home, a folder.
/// Returns its canonical path.
pub fn validate_open(
    _fs: &dyn FileSystem,
    _conf: &Confinement,
    _requested: &str,
) -> std::result::Result<PathBuf, ProjectOpError> {
    // stub: filled by E
    Err(not_implemented())
}

/// Whether `dir` is a Git repository: `dir/.git` exists (a folder, or the
/// file a linked worktree has).
pub fn is_git_repo(fs: &dyn FileSystem, dir: &Path) -> bool {
    fs.exists(&dir.join(".git"))
}
