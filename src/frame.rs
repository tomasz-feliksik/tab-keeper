//! Chrome's native messaging framing: a native-endian `u32` length, then the body.
//!
//! The Unix socket uses the same frames, so one bounded codec serves both sides
//! and a cut-off message can always be told from a finished one.

use std::io::{self, Read, Write};

/// The largest message Chrome accepts from a host. Measured on Chrome for Testing
/// 156: 1,048,576 bytes arrive, and one byte more makes Chrome close the port,
/// which would disconnect the whole profile.
pub const MAX_TO_EXTENSION: u32 = 1 << 20;

/// The largest message this program accepts from an extension. This bound is
/// ours, not Chrome's: far above any tab list, yet still a limit.
pub const MAX_FROM_EXTENSION: u32 = 64 << 20;

const HEADER_LEN: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("a frame of {declared} bytes is over the limit of {limit}")]
    TooLarge { declared: u64, limit: u32 },
    #[error("the stream ended inside a frame")]
    Truncated,
    #[error("reading or writing a frame")]
    Io(#[from] io::Error),
}

/// Reads one frame, or `None` if the stream ended cleanly between frames.
///
/// The body grows as bytes arrive, so a false length cannot make this allocate
/// memory the sender never provided.
pub fn read_frame(reader: &mut impl Read, limit: u32) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = Vec::with_capacity(HEADER_LEN);
    reader
        .by_ref()
        .take(HEADER_LEN as u64)
        .read_to_end(&mut header)?;
    if header.is_empty() {
        return Ok(None);
    }
    let header: [u8; HEADER_LEN] = header.try_into().map_err(|_| FrameError::Truncated)?;
    let declared = u32::from_ne_bytes(header);
    if declared > limit {
        return Err(FrameError::TooLarge {
            declared: declared.into(),
            limit,
        });
    }
    let mut body = Vec::new();
    let received = reader
        .by_ref()
        .take(declared.into())
        .read_to_end(&mut body)?;
    if received as u64 != u64::from(declared) {
        return Err(FrameError::Truncated);
    }
    Ok(Some(body))
}

/// Builds one frame, refusing a body over `limit`.
pub fn encode_frame(body: &[u8], limit: u32) -> Result<Vec<u8>, FrameError> {
    let too_large = || FrameError::TooLarge {
        declared: body.len() as u64,
        limit,
    };
    let declared = u32::try_from(body.len()).map_err(|_| too_large())?;
    if declared > limit {
        return Err(too_large());
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    frame.extend_from_slice(&declared.to_ne_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

/// Writes one frame and flushes it. Nothing is written if the body is over `limit`.
pub fn write_frame(writer: &mut impl Write, body: &[u8], limit: u32) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(body, limit)?)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const LIMIT: u32 = 16;

    fn framed(declared: u32, body: &[u8]) -> Cursor<Vec<u8>> {
        let mut bytes = declared.to_ne_bytes().to_vec();
        bytes.extend_from_slice(body);
        Cursor::new(bytes)
    }

    /// Fails the test if anything tries to read past the header.
    struct HeaderOnly(Cursor<[u8; HEADER_LEN]>);

    impl Read for HeaderOnly {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let read = self.0.read(buffer)?;
            assert!(read > 0, "read past the header");
            Ok(read)
        }
    }

    #[test]
    fn frame_round_trips_with_native_endian_length() {
        let mut written = Vec::new();

        write_frame(&mut written, b"{}", LIMIT).expect("the frame is written");

        assert_eq!(written, [&2u32.to_ne_bytes()[..], b"{}"].concat());
        let body = read_frame(&mut Cursor::new(written), LIMIT).expect("the frame is read");
        assert_eq!(body.as_deref(), Some(&b"{}"[..]));
    }

    #[test]
    fn consecutive_frames_are_read_one_at_a_time() {
        let mut bytes = encode_frame(b"one", LIMIT).expect("encoded");
        bytes.extend(encode_frame(b"", LIMIT).expect("encoded"));
        let mut stream = Cursor::new(bytes);

        let first = read_frame(&mut stream, LIMIT).expect("first frame");
        let second = read_frame(&mut stream, LIMIT).expect("second frame");

        assert_eq!(first.as_deref(), Some(&b"one"[..]));
        assert_eq!(second.as_deref(), Some(&b""[..]));
    }

    #[test]
    fn empty_stream_is_a_clean_end() {
        let end = read_frame(&mut Cursor::new(Vec::new()), LIMIT).expect("a clean end");

        assert_eq!(end, None);
    }

    #[test]
    fn oversized_length_is_refused_before_the_body_is_read() {
        let mut reader = HeaderOnly(Cursor::new((LIMIT + 1).to_ne_bytes()));

        let result = read_frame(&mut reader, LIMIT);

        assert!(matches!(
            result,
            Err(FrameError::TooLarge { declared, limit: LIMIT }) if declared == u64::from(LIMIT) + 1
        ));
    }

    #[test]
    fn largest_length_is_refused_without_a_body() {
        let result = read_frame(&mut framed(u32::MAX, b""), MAX_FROM_EXTENSION);

        assert!(matches!(result, Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn frame_at_the_limit_is_accepted() {
        let body = vec![b'x'; LIMIT as usize];

        let read = read_frame(&mut framed(LIMIT, &body), LIMIT).expect("at the limit");
        let encoded = encode_frame(&body, LIMIT).expect("at the limit");

        assert_eq!(read, Some(body));
        assert_eq!(encoded.len(), LIMIT as usize + HEADER_LEN);
    }

    #[test]
    fn truncated_header_is_an_error() {
        let result = read_frame(&mut Cursor::new(vec![1, 0]), LIMIT);

        assert!(matches!(result, Err(FrameError::Truncated)));
    }

    #[test]
    fn truncated_body_is_an_error() {
        let result = read_frame(&mut framed(4, b"abc"), LIMIT);

        assert!(matches!(result, Err(FrameError::Truncated)));
    }

    #[test]
    fn encoding_over_the_limit_is_refused() {
        let body = vec![b'x'; LIMIT as usize + 1];

        let result = encode_frame(&body, LIMIT);

        assert!(matches!(result, Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn writing_over_the_limit_writes_nothing() {
        let body = vec![b'x'; LIMIT as usize + 1];
        let mut written = Vec::new();

        let result = write_frame(&mut written, &body, LIMIT);

        assert!(matches!(result, Err(FrameError::TooLarge { .. })));
        assert_eq!(written, Vec::<u8>::new());
    }

    #[test]
    fn limits_are_the_measured_chrome_limit_and_our_own_bound() {
        assert_eq!(MAX_TO_EXTENSION, 1_048_576);
        assert_eq!(MAX_FROM_EXTENSION, 67_108_864);
    }
}
