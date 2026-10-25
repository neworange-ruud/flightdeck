//! Images a phone attaches to a reply (remote-control-ty3p).
//!
//! An image crosses the link as [`CommandBody::AttachImageChunk`]s of at most
//! [`MAX_IMAGE_CHUNK_BYTES`], so one envelope stays inside the direct link's
//! message limit. [`ImageUploads`] reassembles them; once the last chunk is in,
//! the host saves the image like a pasted one and parks its path here until the
//! [`CommandBody::Reply`] that names the upload types it into the agent.
//!
//! Both maps are bounded: an upload the phone abandons (the app was killed
//! mid-send) is evicted by later ones instead of growing the host's memory.
//!
//! [`CommandBody::AttachImageChunk`]: flightdeck_remote_protocol::CommandBody::AttachImageChunk
//! [`CommandBody::Reply`]: flightdeck_remote_protocol::CommandBody::Reply

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use flightdeck_remote_protocol::{SessionId, MAX_IMAGE_CHUNK_BYTES};

use crate::tui::clipboard::{pasted_image_extension, PastedImage, MAX_PASTED_IMAGE_BYTES};

/// The most chunks one image may be split into: enough for the largest image
/// the host saves ([`MAX_PASTED_IMAGE_BYTES`]) in full-size chunks.
pub const MAX_IMAGE_CHUNKS: u32 = MAX_PASTED_IMAGE_BYTES.div_ceil(MAX_IMAGE_CHUNK_BYTES) as u32;
/// Uploads reassembled at once; a new one evicts the oldest.
const MAX_PARTIAL_UPLOADS: usize = 4;
/// Saved images waiting for their reply; a new one evicts the oldest.
const MAX_READY_IMAGES: usize = 16;

type Key = (SessionId, String);

/// One image whose chunks are still arriving.
struct Partial {
    format: String,
    chunks: Vec<Option<Vec<u8>>>,
    bytes: usize,
}

/// What one chunk did to its upload.
#[derive(Debug, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// More chunks are needed.
    Pending,
    /// That was the last one: the whole image, ready to save.
    Complete(PastedImage),
}

/// Phone image uploads in flight, and saved ones awaiting their reply.
#[derive(Default)]
pub struct ImageUploads {
    partial: HashMap<Key, Partial>,
    partial_order: VecDeque<Key>,
    ready: HashMap<Key, PathBuf>,
    ready_order: VecDeque<Key>,
}

impl ImageUploads {
    /// Add chunk `index` of `count` of `session`'s upload `upload_id`. Refused,
    /// with the reason the phone shows, when the chunk does not fit the upload
    /// it belongs to or the image would exceed what the host saves.
    pub fn add_chunk(
        &mut self,
        session: &SessionId,
        upload_id: &str,
        format: &str,
        index: u32,
        count: u32,
        data: Vec<u8>,
    ) -> Result<ChunkOutcome, String> {
        if pasted_image_extension(format).is_none() {
            return Err(format!(
                "a {format} image cannot be attached; send PNG or JPEG"
            ));
        }
        if count == 0 || count > MAX_IMAGE_CHUNKS || index >= count {
            return Err("the image arrived malformed; attach it again".to_string());
        }
        if data.is_empty() || data.len() > MAX_IMAGE_CHUNK_BYTES {
            return Err("the image arrived malformed; attach it again".to_string());
        }
        let key = (session.clone(), upload_id.to_string());
        if !self.partial.contains_key(&key) {
            if self.partial_order.len() >= MAX_PARTIAL_UPLOADS {
                if let Some(oldest) = self.partial_order.pop_front() {
                    self.partial.remove(&oldest);
                }
            }
            self.partial_order.push_back(key.clone());
            self.partial.insert(
                key.clone(),
                Partial {
                    format: format.to_string(),
                    chunks: vec![None; count as usize],
                    bytes: 0,
                },
            );
        }
        let upload = self.partial.get_mut(&key).expect("inserted above");
        if upload.format != format || upload.chunks.len() != count as usize {
            self.drop_partial(&key);
            return Err("the image arrived malformed; attach it again".to_string());
        }
        // A retransmitted chunk replaces its earlier copy.
        let slot = &mut upload.chunks[index as usize];
        upload.bytes -= slot.as_ref().map_or(0, Vec::len);
        upload.bytes += data.len();
        *slot = Some(data);
        if upload.bytes > MAX_PASTED_IMAGE_BYTES {
            self.drop_partial(&key);
            return Err(format!(
                "the image is larger than {} MB",
                MAX_PASTED_IMAGE_BYTES / (1024 * 1024)
            ));
        }
        if upload.chunks.iter().any(Option::is_none) {
            return Ok(ChunkOutcome::Pending);
        }
        let upload = self.drop_partial(&key).expect("present");
        Ok(ChunkOutcome::Complete(PastedImage {
            format: upload.format,
            bytes: upload.chunks.into_iter().flatten().flatten().collect(),
        }))
    }

    /// Park the saved image of `session`'s upload `upload_id` for its reply.
    pub fn finish(&mut self, session: &SessionId, upload_id: &str, path: PathBuf) {
        let key = (session.clone(), upload_id.to_string());
        if self.ready.insert(key.clone(), path).is_none() {
            if self.ready_order.len() >= MAX_READY_IMAGES {
                if let Some(oldest) = self.ready_order.pop_front() {
                    self.ready.remove(&oldest);
                }
            }
            self.ready_order.push_back(key);
        }
    }

    /// Whether `session`'s upload `upload_id` is saved and waiting.
    pub fn is_ready(&self, session: &SessionId, upload_id: &str) -> bool {
        self.ready
            .contains_key(&(session.clone(), upload_id.to_string()))
    }

    /// The saved image of `session`'s upload `upload_id`, handed over once.
    pub fn take(&mut self, session: &SessionId, upload_id: &str) -> Option<PathBuf> {
        let key = (session.clone(), upload_id.to_string());
        self.ready_order.retain(|k| k != &key);
        self.ready.remove(&key)
    }

    fn drop_partial(&mut self, key: &Key) -> Option<Partial> {
        self.partial_order.retain(|k| k != key);
        self.partial.remove(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::new("s")
    }

    #[test]
    fn chunks_in_any_order_reassemble_the_image() {
        let mut up = ImageUploads::default();
        assert_eq!(
            up.add_chunk(&sid(), "u", "jpg", 1, 3, vec![2, 3]),
            Ok(ChunkOutcome::Pending)
        );
        assert_eq!(
            up.add_chunk(&sid(), "u", "jpg", 0, 3, vec![0, 1]),
            Ok(ChunkOutcome::Pending)
        );
        // A retransmitted chunk replaces, never duplicates.
        assert_eq!(
            up.add_chunk(&sid(), "u", "jpg", 0, 3, vec![0, 1]),
            Ok(ChunkOutcome::Pending)
        );
        assert_eq!(
            up.add_chunk(&sid(), "u", "jpg", 2, 3, vec![4]),
            Ok(ChunkOutcome::Complete(PastedImage {
                format: "jpg".to_string(),
                bytes: vec![0, 1, 2, 3, 4],
            }))
        );
    }

    #[test]
    fn a_saved_image_is_handed_over_once_per_session() {
        let mut up = ImageUploads::default();
        up.finish(&sid(), "u", PathBuf::from("/tmp/a.jpg"));
        assert!(up.is_ready(&sid(), "u"));
        assert!(!up.is_ready(&SessionId::new("other"), "u"));
        assert_eq!(up.take(&sid(), "u"), Some(PathBuf::from("/tmp/a.jpg")));
        assert_eq!(up.take(&sid(), "u"), None);
    }

    #[test]
    fn malformed_and_oversized_uploads_are_refused() {
        let mut up = ImageUploads::default();
        let bad = |r: Result<ChunkOutcome, String>| r.unwrap_err();
        assert!(bad(up.add_chunk(&sid(), "u", "svg", 0, 1, vec![1])).contains("svg"));
        bad(up.add_chunk(&sid(), "u", "png", 1, 1, vec![1]));
        bad(up.add_chunk(&sid(), "u", "png", 0, 0, vec![1]));
        bad(up.add_chunk(&sid(), "u", "png", 0, MAX_IMAGE_CHUNKS + 1, vec![1]));
        bad(up.add_chunk(&sid(), "u", "png", 0, 1, Vec::new()));
        bad(up.add_chunk(&sid(), "u", "png", 0, 1, vec![0; MAX_IMAGE_CHUNK_BYTES + 1]));
        // A chunk that disagrees with its upload drops the upload.
        up.add_chunk(&sid(), "v", "png", 0, 2, vec![1]).unwrap();
        bad(up.add_chunk(&sid(), "v", "jpg", 1, 2, vec![1]));
        assert_eq!(
            up.add_chunk(&sid(), "v", "png", 1, 2, vec![1]),
            Ok(ChunkOutcome::Pending),
            "the mismatched upload was dropped and starts over"
        );
    }

    #[test]
    fn abandoned_uploads_are_evicted_oldest_first() {
        let mut up = ImageUploads::default();
        for i in 0..=MAX_PARTIAL_UPLOADS {
            up.add_chunk(&sid(), &format!("u{i}"), "png", 0, 2, vec![1])
                .unwrap();
        }
        // u0 was evicted: its second chunk starts a fresh upload.
        assert_eq!(
            up.add_chunk(&sid(), "u0", "png", 1, 2, vec![1]),
            Ok(ChunkOutcome::Pending)
        );
        for i in 0..=MAX_READY_IMAGES {
            up.finish(&sid(), &format!("r{i}"), PathBuf::from(format!("/r{i}")));
        }
        assert!(!up.is_ready(&sid(), "r0"), "oldest saved image evicted");
        assert!(up.is_ready(&sid(), &format!("r{MAX_READY_IMAGES}")));
    }
}
