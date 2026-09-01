//! Length-delimited JSON framing.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Refuse to allocate for an absurd length prefix. Comfortably above the
/// default 8 MiB `max_item_bytes` plus JSON overhead.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// The peer sent a length we will not allocate for.
    TooLarge(u32),
    Json(serde_json::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "io error: {e}"),
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds the {MAX_FRAME_BYTES} byte limit"),
            FrameError::Json(e) => write!(f, "malformed message: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(e: serde_json::Error) -> Self {
        FrameError::Json(e)
    }
}

pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value)?;
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame. `Ok(None)` means the peer closed cleanly between frames.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<Option<T>, FrameError> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(FrameError::Io(e)),
    }
    let len = u32::from_be_bytes(header);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_back_to_back() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &"first").unwrap();
        write_frame(&mut buffer, &"second").unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        assert_eq!(read_frame::<_, String>(&mut cursor).unwrap().as_deref(), Some("first"));
        assert_eq!(read_frame::<_, String>(&mut cursor).unwrap().as_deref(), Some("second"));
        assert_eq!(read_frame::<_, String>(&mut cursor).unwrap(), None);
    }

    #[test]
    fn a_hostile_length_prefix_is_refused_before_allocating() {
        let mut input = std::io::Cursor::new(0xffff_ffffu32.to_be_bytes().to_vec());
        assert!(matches!(
            read_frame::<_, String>(&mut input),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_silent_close() {
        let mut input = std::io::Cursor::new([0, 0, 0, 8, b'x'].to_vec());
        assert!(matches!(read_frame::<_, String>(&mut input), Err(FrameError::Io(_))));
    }
}
