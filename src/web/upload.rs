//! Uploading files to a worker (v8, `remote-control-usem`): the grants.
//!
//! A worker's window asks over the WebSocket
//! ([`crate::web::protocol::worker_command::BEGIN_UPLOAD`]); the worker host
//! checks the folder, the names and the free space, and hands the server one
//! [`UploadGrant`] per file through
//! [`crate::web::server::WebServerHandle::grant_uploads`]. The window then
//! sends each file's bytes over plain HTTP, `PUT /upload/{ticket}` — the
//! route lives with the server (`server/upload_route.rs`, unix only, like the
//! worker it serves), because it is one more door that checks the cookie.
//!
//! A grant is the whole authority for one write:
//!
//! * **bound to one credential** — the cookie the asking window authenticated
//!   with. Any other cookie, valid or not, is answered as if the ticket did
//!   not exist;
//! * **for one file** — its folder (canonical, inside `$HOME`), its final
//!   name, its exact size, and whether it may replace what is there;
//! * **single use** — redeemed before the first byte is written, so a second
//!   `PUT` with the same ticket finds nothing;
//! * **short-lived** — unused for [`UPLOAD_GRANT_TTL_MS`], it is gone. Expired
//!   grants are pruned lazily (on every grant and redemption) and by the
//!   host's sweep ([`UploadGrants::expire`]), and each one is reported to the
//!   host as a failed [`UploadFinished`], so a batch always completes.
//!
//! Tickets are 128 random bits from the OS (the same source as the access
//! tokens), base64url, so a ticket names a grant and cannot be guessed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};

use crate::web::credentials::TokenId;

/// The upload route's path prefix: a file goes to `/upload/{ticket}`.
pub const UPLOAD_PATH: &str = "/upload";

/// How long an unused grant lives: two minutes of the server's clock.
pub const UPLOAD_GRANT_TTL_MS: i64 = 120_000;

/// How many random bytes a ticket carries (128 bits).
const TICKET_BYTES: usize = 16;

/// One file the host grants an upload for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadGrantRequest {
    /// The name the file will have in the folder.
    pub final_name: String,
    /// Its exact size: the `PUT`'s `Content-Length` must say this.
    pub size: u64,
    /// Whether it may overwrite a file of that name (`on_conflict:
    /// "replace"`). Otherwise the write fails if the name is taken by then.
    pub replace: bool,
}

/// How one grant ended, for the host (`WebInbound::UploadFinished`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadFinished {
    /// The credential the grant was bound to.
    pub token: TokenId,
    /// The host's batch id, as it passed it to the grant.
    pub batch: u64,
    /// The file's final path on the worker.
    pub path: PathBuf,
    /// The bytes written (the file's size, on success).
    pub bytes: u64,
    /// Whether the file is now there.
    pub ok: bool,
    /// Why not, in words for a person, when it is not.
    pub reason: Option<String>,
}

/// One outstanding grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadGrant {
    /// The URL-safe ticket that names it.
    pub ticket: String,
    /// The credential that may redeem it.
    pub token: TokenId,
    /// The host's batch id, echoed in [`UploadFinished`].
    pub batch: u64,
    /// The confinement root (`$HOME`, canonical): the folder must still be
    /// inside it when the file is committed.
    pub home: PathBuf,
    /// The target folder, canonical.
    pub dir: PathBuf,
    /// The file's name in it.
    pub final_name: String,
    /// The exact size.
    pub size: u64,
    /// Whether it may overwrite.
    pub replace: bool,
    /// The server clock (unix ms) after which it is gone.
    pub expires_ms: i64,
}

impl UploadGrant {
    /// The file's final path.
    pub fn final_path(&self) -> PathBuf {
        self.dir.join(&self.final_name)
    }

    /// The failed [`UploadFinished`] for this grant.
    pub fn failed(&self, reason: impl Into<String>, bytes: u64) -> UploadFinished {
        UploadFinished {
            token: self.token.clone(),
            batch: self.batch,
            path: self.final_path(),
            bytes,
            ok: false,
            reason: Some(reason.into()),
        }
    }
}

/// What a `PUT` presented, judged against the grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redeem {
    /// The grant, now spent: write the file.
    Granted(UploadGrant),
    /// No such ticket for this credential: unknown, expired, already used, or
    /// someone else's. One answer for all four, so a ticket's existence is
    /// never confirmed to a credential it was not granted to.
    Invalid,
    /// The ticket is good but the length is not the granted size. The grant is
    /// kept: the client may try again with the right length.
    SizeMismatch {
        /// The granted size.
        expected: u64,
    },
}

/// The outstanding grants, keyed by ticket.
#[derive(Debug, Default)]
pub struct UploadGrants {
    grants: HashMap<String, UploadGrant>,
}

impl UploadGrants {
    /// Grant one upload per request, into canonical `dir` under canonical
    /// `home`, for `token`, as part of the host's `batch`. Returns the
    /// tickets, in request order. Expired grants are pruned first and
    /// returned too, for the host to be told.
    pub fn grant(
        &mut self,
        token: &TokenId,
        batch: u64,
        home: &Path,
        dir: &Path,
        files: &[UploadGrantRequest],
        now_ms: i64,
    ) -> (Vec<String>, Vec<UploadGrant>) {
        let expired = self.expire(now_ms);
        let tickets = files
            .iter()
            .map(|file| {
                let mut ticket = new_ticket();
                while self.grants.contains_key(&ticket) {
                    ticket = new_ticket();
                }
                self.grants.insert(
                    ticket.clone(),
                    UploadGrant {
                        ticket: ticket.clone(),
                        token: token.clone(),
                        batch,
                        home: home.to_path_buf(),
                        dir: dir.to_path_buf(),
                        final_name: file.final_name.clone(),
                        size: file.size,
                        replace: file.replace,
                        expires_ms: now_ms.saturating_add(UPLOAD_GRANT_TTL_MS),
                    },
                );
                ticket
            })
            .collect();
        (tickets, expired)
    }

    /// Remove and return every grant whose time is up.
    pub fn expire(&mut self, now_ms: i64) -> Vec<UploadGrant> {
        let due: Vec<String> = self
            .grants
            .iter()
            .filter(|(_, g)| now_ms > g.expires_ms)
            .map(|(ticket, _)| ticket.clone())
            .collect();
        let mut expired: Vec<UploadGrant> = due
            .iter()
            .filter_map(|ticket| self.grants.remove(ticket))
            .collect();
        expired.sort_by(|a, b| a.final_name.cmp(&b.final_name));
        expired
    }

    /// How many grants are outstanding.
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Judge a `PUT` of `content_length` bytes to `ticket` by `token`, and
    /// spend the grant when it passes. Expired grants are pruned first and
    /// returned, for the host to be told.
    #[cfg(unix)]
    pub fn redeem(
        &mut self,
        ticket: &str,
        token: &TokenId,
        content_length: u64,
        now_ms: i64,
    ) -> (Redeem, Vec<UploadGrant>) {
        let expired = self.expire(now_ms);
        let verdict = match self.grants.get(ticket) {
            Some(grant) if &grant.token != token => Redeem::Invalid,
            Some(grant) if grant.size != content_length => Redeem::SizeMismatch {
                expected: grant.size,
            },
            Some(_) => match self.grants.remove(ticket) {
                Some(grant) => Redeem::Granted(grant),
                None => Redeem::Invalid,
            },
            None => Redeem::Invalid,
        };
        (verdict, expired)
    }
}

/// A byte count for a person: `512 B`, `3.4 KB`, `12 MB`, `1.5 GB`
/// (powers of 1024, at most one decimal, none from 10 up).
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{} {}", value.round() as u64, UNITS[unit])
    } else {
        let tenths = (value * 10.0).round() / 10.0;
        if tenths.fract() == 0.0 {
            format!("{} {}", tenths as u64, UNITS[unit])
        } else {
            format!("{tenths:.1} {}", UNITS[unit])
        }
    }
}

/// A fresh ticket: 128 random bits, base64url without padding (22 chars).
fn new_ticket() -> String {
    let mut bytes = [0u8; TICKET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests;
