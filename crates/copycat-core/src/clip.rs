//! Clipboard values and the events that record them.
//!
//! A `ClipPayload` is a set of representations of one copy — `text/plain` plus
//! `text/html` plus an image are one payload, not three. Nothing here knows how
//! a payload reached the clipboard or where it is stored.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const TEXT_PLAIN: &str = "text/plain";
pub const TEXT_HTML: &str = "text/html";

/// Identifier for a recorded copy event. Monotonic within a daemon lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipId(pub u64);

impl fmt::Display for ClipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ClipId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(ClipId)
    }
}

/// Content-addressed fingerprint of a payload.
///
/// Used for duplicate policy (R1) and for self-write suppression (R16). It is
/// computed over normalized representations so that two payloads carrying the
/// same representations in a different order hash identically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(#[serde(with = "hash_hex")] pub [u8; 32]);

impl ContentHash {
    /// Short form for logs and errors. Never log a payload; log this (§23.3).
    pub fn prefix(&self) -> String {
        self.0[..6].iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(ContentHash(out))
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.prefix())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

mod hash_hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let hex = String::deserialize(d)?;
        super::ContentHash::from_hex(&hex)
            .map(|h| h.0)
            .ok_or_else(|| D::Error::custom("expected 64 hex characters"))
    }
}

/// Whether a clipboard change came from the outside world or from us.
///
/// Only `External` values are recorded as history (§10). `Internal` exists so
/// the classification is explicit in the type system rather than implied by a
/// missing record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipSource {
    External,
    Internal,
}

/// One encoding of a clipboard value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Representation {
    pub media_type: String,
    #[serde(with = "byte_vec")]
    pub bytes: Vec<u8>,
}

mod byte_vec {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize UTF-8 payloads as strings so protocol traffic and stored blobs
    /// stay inspectable; fall back to a byte array for binary representations.
    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(bytes) {
            Ok(text) => s.serialize_str(text),
            Err(_) => s.collect_seq(bytes.iter()),
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Bytes(Vec<u8>),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        Ok(match Either::deserialize(d)? {
            Either::Text(t) => t.into_bytes(),
            Either::Bytes(b) => b,
        })
    }
}

/// A clipboard value: every representation the source offered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClipPayload {
    pub representations: Vec<Representation>,
}

impl ClipPayload {
    pub fn text(value: impl Into<String>) -> Self {
        ClipPayload {
            representations: vec![Representation {
                media_type: TEXT_PLAIN.to_string(),
                bytes: value.into().into_bytes(),
            }],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.representations.iter().all(|r| r.bytes.is_empty())
    }

    /// The `text/plain` representation, if the payload has one and it is UTF-8.
    pub fn as_text(&self) -> Option<&str> {
        self.representations
            .iter()
            .find(|r| r.media_type == TEXT_PLAIN)
            .and_then(|r| std::str::from_utf8(&r.bytes).ok())
    }

    pub fn media_types(&self) -> Vec<String> {
        self.representations
            .iter()
            .map(|r| r.media_type.clone())
            .collect()
    }

    pub fn byte_len(&self) -> usize {
        self.representations.iter().map(|r| r.bytes.len()).sum()
    }

    /// Stable across representation ordering: representations are sorted by
    /// media type before hashing, and each is length-prefixed so that
    /// `("ab", "c")` and `("a", "bc")` cannot collide.
    pub fn content_hash(&self) -> ContentHash {
        let mut ordered: Vec<&Representation> = self.representations.iter().collect();
        ordered.sort_by(|a, b| a.media_type.cmp(&b.media_type).then(a.bytes.cmp(&b.bytes)));

        let mut hasher = blake3::Hasher::new();
        hasher.update(&(ordered.len() as u64).to_le_bytes());
        for repr in ordered {
            hasher.update(&(repr.media_type.len() as u64).to_le_bytes());
            hasher.update(repr.media_type.as_bytes());
            hasher.update(&(repr.bytes.len() as u64).to_le_bytes());
            hasher.update(&repr.bytes);
        }
        ContentHash(*hasher.finalize().as_bytes())
    }

    /// A short, single-line, payload-safe excerpt for list views.
    pub fn preview(&self, max_chars: usize) -> String {
        match self.as_text() {
            Some(text) => {
                let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut out: String = flat.chars().take(max_chars).collect();
                if flat.chars().count() > max_chars {
                    out.push('…');
                }
                out
            }
            None => format!(
                "<{}, {} bytes>",
                self.media_types().join(", "),
                self.byte_len()
            ),
        }
    }
}

/// An external copy, recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipEvent {
    pub id: ClipId,
    pub captured_at: i64,
    pub source: ClipSource,
    pub content_hash: ContentHash,
    pub payload: ClipPayload,
    pub pinned: bool,
}

impl ClipEvent {
    pub fn summary(&self, preview_chars: usize) -> ClipSummary {
        ClipSummary {
            id: self.id,
            captured_at: self.captured_at,
            content_hash: self.content_hash,
            media_types: self.payload.media_types(),
            byte_len: self.payload.byte_len(),
            preview: self.payload.preview(preview_chars),
            pinned: self.pinned,
            duplicate_run: 1,
        }
    }
}

/// What list views send over the wire: metadata plus a bounded preview, never
/// the whole payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipSummary {
    pub id: ClipId,
    pub captured_at: i64,
    pub content_hash: ContentHash,
    pub media_types: Vec<String>,
    pub byte_len: usize,
    pub preview: String,
    pub pinned: bool,
    /// How many consecutive raw events this logical entry stands for. Always 1
    /// under `preserve`; the run length under `collapse` (§12).
    pub duplicate_run: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_order_independent_across_representations() {
        let a = ClipPayload {
            representations: vec![
                Representation { media_type: TEXT_PLAIN.into(), bytes: b"hi".to_vec() },
                Representation { media_type: TEXT_HTML.into(), bytes: b"<b>hi</b>".to_vec() },
            ],
        };
        let b = ClipPayload {
            representations: vec![
                Representation { media_type: TEXT_HTML.into(), bytes: b"<b>hi</b>".to_vec() },
                Representation { media_type: TEXT_PLAIN.into(), bytes: b"hi".to_vec() },
            ],
        };
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn length_prefixing_prevents_concatenation_collisions() {
        let a = ClipPayload {
            representations: vec![Representation {
                media_type: "text/ab".into(),
                bytes: b"c".to_vec(),
            }],
        };
        let b = ClipPayload {
            representations: vec![Representation {
                media_type: "text/a".into(),
                bytes: b"bc".to_vec(),
            }],
        };
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn distinct_text_hashes_differently() {
        assert_ne!(
            ClipPayload::text("alpha").content_hash(),
            ClipPayload::text("beta").content_hash()
        );
        assert_eq!(
            ClipPayload::text("alpha").content_hash(),
            ClipPayload::text("alpha").content_hash()
        );
    }

    #[test]
    fn hash_hex_round_trips() {
        let h = ClipPayload::text("x").content_hash();
        assert_eq!(ContentHash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(ContentHash::from_hex("nope"), None);
    }

    #[test]
    fn preview_flattens_and_truncates() {
        let p = ClipPayload::text("one\n  two\tthree four");
        assert_eq!(p.preview(80), "one two three four");
        assert_eq!(p.preview(7), "one two…");
    }

    #[test]
    fn preview_of_binary_names_the_type_not_the_bytes() {
        let p = ClipPayload {
            representations: vec![Representation {
                media_type: "image/png".into(),
                bytes: vec![0u8; 4096],
            }],
        };
        assert_eq!(p.preview(20), "<image/png, 4096 bytes>");
    }

    #[test]
    fn binary_representations_survive_a_serde_round_trip() {
        let p = ClipPayload {
            representations: vec![Representation {
                media_type: "image/png".into(),
                bytes: vec![0x89, 0x50, 0x4e, 0xff, 0xfe],
            }],
        };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<ClipPayload>(&json).unwrap(), p);
    }
}
