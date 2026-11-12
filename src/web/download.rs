//! Downloading files from a worker (v9, `remote-control-pz6d`): the grants.
//!
//! A worker's window asks over the WebSocket
//! ([`crate::web::protocol::worker_command::BEGIN_DOWNLOAD`]); the worker host
//! checks the folder and each file, and hands the server one
//! [`DownloadGrant`] per file through
//! [`crate::web::server::WebServerHandle::grant_downloads`]. The window then
//! fetches each file over plain HTTP, `GET /download/{ticket}` — the route
//! lives with the server (`server/download_route.rs`, unix only, like the
//! worker it serves), because it is one more door that checks the cookie.
//!
//! A grant is the whole authority for one read, on the same terms as an
//! upload's ([`crate::web::upload`]):
//!
//! * **bound to one credential** — the cookie the asking window authenticated
//!   with. Any other cookie, valid or not, is answered as if the ticket did
//!   not exist, and spends nothing;
//! * **for one file** — its canonical path inside `$HOME` (canonical), the
//!   name to suggest, and its size when granted. The route resolves the path
//!   again and refuses a file that has moved, become a link or left home;
//! * **single use** — redeemed when the request starts, so a second `GET`
//!   with the same ticket finds nothing;
//! * **short-lived** — unused for [`DOWNLOAD_GRANT_TTL_MS`] (an upload's two
//!   minutes), it is gone, pruned lazily on every grant and redemption.
//!   Nothing is reported for it: a download changes nothing on the worker.
//!
//! Tickets come from the same generator as upload tickets: 128 random bits
//! from the OS, base64url.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::web::credentials::TokenId;
use crate::web::upload::{new_ticket, UPLOAD_GRANT_TTL_MS};

/// The download route's path prefix: a file comes from `/download/{ticket}`.
pub const DOWNLOAD_PATH: &str = "/download";

/// How long an unused grant lives: the same two minutes as an upload's.
pub const DOWNLOAD_GRANT_TTL_MS: i64 = UPLOAD_GRANT_TTL_MS;

/// One file the host grants a download of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadGrantRequest {
    /// The file, canonical, inside the grant's home.
    pub path: PathBuf,
    /// The name the download suggests (the name asked for).
    pub name: String,
    /// Its size when granted.
    pub size: u64,
}

/// One outstanding grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadGrant {
    /// The URL-safe ticket that names it.
    pub ticket: String,
    /// The credential that may redeem it.
    pub token: TokenId,
    /// The confinement root (`$HOME`, canonical): the file must still be
    /// inside it when it is read.
    pub home: PathBuf,
    /// The file, canonical.
    pub path: PathBuf,
    /// The name the download suggests.
    pub name: String,
    /// Its size when granted.
    pub size: u64,
    /// The server clock (unix ms) after which it is gone.
    pub expires_ms: i64,
}

/// The outstanding grants, keyed by ticket.
#[derive(Debug, Default)]
pub struct DownloadGrants {
    grants: HashMap<String, DownloadGrant>,
}

impl DownloadGrants {
    /// Grant one download per request, under canonical `home`, for `token`.
    /// Returns the tickets, in request order. Expired grants are pruned first.
    pub fn grant(
        &mut self,
        token: &TokenId,
        home: &Path,
        files: &[DownloadGrantRequest],
        now_ms: i64,
    ) -> Vec<String> {
        self.expire(now_ms);
        files
            .iter()
            .map(|file| {
                let mut ticket = new_ticket();
                while self.grants.contains_key(&ticket) {
                    ticket = new_ticket();
                }
                self.grants.insert(
                    ticket.clone(),
                    DownloadGrant {
                        ticket: ticket.clone(),
                        token: token.clone(),
                        home: home.to_path_buf(),
                        path: file.path.clone(),
                        name: file.name.clone(),
                        size: file.size,
                        expires_ms: now_ms.saturating_add(DOWNLOAD_GRANT_TTL_MS),
                    },
                );
                ticket
            })
            .collect()
    }

    /// Drop every grant whose time is up; returns how many went.
    pub fn expire(&mut self, now_ms: i64) -> usize {
        let before = self.grants.len();
        self.grants.retain(|_, grant| now_ms <= grant.expires_ms);
        before - self.grants.len()
    }

    /// How many grants are outstanding.
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Spend the grant `ticket` names, when `token` holds it and it is still
    /// live. `None` — unknown, expired, already used, or someone else's — is
    /// one answer for all four, so a ticket's existence is never confirmed to
    /// a credential it was not granted to; someone else's is kept for its
    /// owner.
    #[cfg(unix)]
    pub fn redeem(&mut self, ticket: &str, token: &TokenId, now_ms: i64) -> Option<DownloadGrant> {
        self.expire(now_ms);
        match self.grants.get(ticket) {
            Some(grant) if &grant.token == token => self.grants.remove(ticket),
            _ => None,
        }
    }
}

/// A `Content-Disposition` that saves the body as `name`: an ASCII
/// `filename` for clients that know nothing else (quotes, backslashes,
/// control and non-ASCII characters replaced with `_`) and the exact name as
/// `filename*` (RFC 6266 / RFC 5987, UTF-8, percent-encoded), which every
/// current browser prefers.
pub fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        rfc5987_encode(name)
    )
}

/// `text` percent-encoded for an RFC 5987 `ext-value`: everything but
/// `attr-char` (ASCII letters, digits and ``!#$&+-.^_`|~``) as `%XX` of its
/// UTF-8 bytes.
fn rfc5987_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests;
