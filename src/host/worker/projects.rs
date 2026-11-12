//! Opening projects on a worker (`specs/WORKER_PRD.md` §4.4, W9/W16):
//! browse from `$HOME` down, scan `projects_root`, open a path.
//!
//! Everything is confined to `$HOME`: a requested path is canonicalised and
//! refused when it resolves outside it, including through a symlink.
//!
//! A listing can carry the folder's regular files too (v9, the Transfer files
//! panel's Download tab): [`browse`] with `include_files`.

use std::path::{Path, PathBuf};

use crate::contracts::{FileSystem, Result};

/// The most entries one listing returns — folders and files together,
/// folders first; more sets `truncated`.
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
    pub fn new(fs: &dyn FileSystem, home: &Path) -> Result<Confinement> {
        Ok(Confinement {
            home: fs.canonicalize(home)?,
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
        fs: &dyn FileSystem,
        requested: Option<&str>,
    ) -> std::result::Result<PathBuf, ProjectOpError> {
        let text = requested.map(str::trim).unwrap_or("");
        let joined = match text {
            "" | "~" => return Ok(self.home.clone()),
            _ => match text.strip_prefix("~/") {
                Some(rest) => self.home.join(rest),
                None => {
                    let p = Path::new(text);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        self.home.join(p)
                    }
                }
            },
        };
        match fs.canonicalize(&joined) {
            Ok(canonical) if canonical.starts_with(&self.home) => Ok(canonical),
            Ok(_) => Err(ProjectOpError::OutsideHome(text.to_string())),
            // A path that is missing and would not be inside home anyway is
            // "outside": do not tell a caller what exists beyond home.
            Err(_) if !lexically_normal(&joined).starts_with(&self.home) => {
                Err(ProjectOpError::OutsideHome(text.to_string()))
            }
            Err(_) => Err(ProjectOpError::NotFound(text.to_string())),
        }
    }

    /// The canonical form of `p` when it is inside home, else `None`.
    fn inside(&self, fs: &dyn FileSystem, p: &Path) -> Option<PathBuf> {
        fs.canonicalize(p)
            .ok()
            .filter(|c| c.starts_with(&self.home))
    }
}

/// `p` with `.` and `..` folded away without touching the filesystem.
pub(super) fn lexically_normal(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One folder of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_git_repo: bool,
}

/// One regular file of a listing (v9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    /// The path as listed (a link is not resolved).
    pub path: PathBuf,
    /// Its size in bytes when listed.
    pub size: u64,
}

/// The folders (and, when asked, the files) inside one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub path: PathBuf,
    /// `None` at home: browsing never goes above it.
    pub parent: Option<PathBuf>,
    pub entries: Vec<Entry>,
    /// The regular files, only when `include_files`; empty otherwise.
    pub files: Vec<FileEntry>,
    pub truncated: bool,
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// One child of a folder: `(name, path as listed, canonical path)`.
type Child = (String, PathBuf, PathBuf);

/// The children of canonical `dir` that stay inside home, hidden ones only
/// when `show_hidden`: its sub-folders, and — when `include_files` — its
/// regular files, each list sorted case-insensitively by name.
fn children(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &Path,
    show_hidden: bool,
    include_files: bool,
) -> std::result::Result<(Vec<Child>, Vec<Child>), ProjectOpError> {
    let names = fs
        .list_dir(dir)
        .map_err(|e| ProjectOpError::Io(e.to_string()))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for path in names {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if !show_hidden && is_hidden(&name) {
            continue;
        }
        // A link that leaves home or dangles is neither a navigable folder
        // nor a file to download.
        let Some(canonical) = conf.inside(fs, &path) else {
            continue;
        };
        if fs.is_dir(&canonical) {
            dirs.push((name, path, canonical));
        } else if include_files && fs.is_file(&canonical) {
            files.push((name, path, canonical));
        }
    }
    let by_name = |a: &Child, b: &Child| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    Ok((dirs, files))
}

/// The sub-folders of canonical `dir` that stay inside home ([`children`]).
fn child_dirs(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &Path,
    show_hidden: bool,
) -> std::result::Result<Vec<Child>, ProjectOpError> {
    children(fs, conf, dir, show_hidden, false).map(|(dirs, _)| dirs)
}

/// List the folders in `requested` (hidden ones only when `show_hidden`,
/// sorted case-insensitively) and, with `include_files`, its regular files
/// the same way: a link is listed when it resolves to a regular file inside
/// home. At most [`MAX_ENTRIES`] in all, folders first.
pub fn browse(
    fs: &dyn FileSystem,
    conf: &Confinement,
    requested: Option<&str>,
    show_hidden: bool,
    include_files: bool,
) -> std::result::Result<Listing, ProjectOpError> {
    let path = conf.resolve(fs, requested)?;
    if !fs.is_dir(&path) {
        return Err(ProjectOpError::NotADirectory(
            requested.unwrap_or("~").to_string(),
        ));
    }
    let (dirs, file_children) = children(fs, conf, &path, show_hidden, include_files)?;
    let mut truncated = dirs.len() > MAX_ENTRIES;
    let entries: Vec<Entry> = dirs
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|(name, listed, canonical)| Entry {
            name,
            path: listed,
            is_git_repo: is_git_repo(fs, &canonical),
        })
        .collect();
    let room = MAX_ENTRIES - entries.len();
    truncated |= file_children.len() > room;
    let files: Vec<FileEntry> = file_children
        .into_iter()
        .take(room)
        // A file that went away between the listing and its size is left out.
        .filter_map(|(name, listed, canonical)| {
            let size = fs.file_len(&canonical).ok()?;
            Some(FileEntry {
                name,
                path: listed,
                size,
            })
        })
        .collect();
    let parent = if path == conf.home {
        None
    } else {
        path.parent().map(Path::to_path_buf)
    };
    Ok(Listing {
        path,
        parent,
        entries,
        files,
        truncated,
    })
}

/// The repositories under `root`, `depth` levels deep (a repository is not
/// descended into; hidden folders are skipped; a missing root is empty).
pub fn scan(
    fs: &dyn FileSystem,
    conf: &Confinement,
    root: &Path,
    depth: usize,
) -> std::result::Result<Vec<Entry>, ProjectOpError> {
    let root = match conf.resolve(fs, Some(&root.to_string_lossy())) {
        Ok(root) => root,
        Err(ProjectOpError::OutsideHome(p)) => return Err(ProjectOpError::RootOutsideHome(p)),
        Err(ProjectOpError::NotFound(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    if !fs.is_dir(&root) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    scan_level(fs, conf, &root, depth, &mut found);
    found.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(found)
}

fn scan_level(
    fs: &dyn FileSystem,
    conf: &Confinement,
    dir: &Path,
    levels_left: usize,
    found: &mut Vec<Entry>,
) {
    if levels_left == 0 {
        return;
    }
    // An unreadable folder is skipped, not fatal to the whole scan.
    let Ok(children) = child_dirs(fs, conf, dir, false) else {
        return;
    };
    for (name, listed, canonical) in children {
        if is_git_repo(fs, &canonical) {
            found.push(Entry {
                name,
                path: listed,
                is_git_repo: true,
            });
        } else {
            scan_level(fs, conf, &canonical, levels_left - 1, found);
        }
    }
}

/// Check that `requested` may be opened as a project: inside home, a folder.
/// Returns its canonical path.
pub fn validate_open(
    fs: &dyn FileSystem,
    conf: &Confinement,
    requested: &str,
) -> std::result::Result<PathBuf, ProjectOpError> {
    let path = conf.resolve(fs, Some(requested))?;
    if !fs.is_dir(&path) {
        return Err(ProjectOpError::NotADirectory(requested.to_string()));
    }
    Ok(path)
}

/// Whether `dir` is a Git repository: `dir/.git` exists (a folder, or the
/// file a linked worktree has).
pub fn is_git_repo(fs: &dyn FileSystem, dir: &Path) -> bool {
    fs.exists(&dir.join(".git"))
}

// The worker is Linux and macOS only (W2); its tests use unix paths.
#[cfg(all(test, unix))]
mod tests;
