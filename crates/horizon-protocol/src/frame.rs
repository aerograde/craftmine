//! Packet framing (Varint21FrameDecoder / Varint21LengthFieldPrepender) and
//! zlib compression above a threshold (CompressionEncoder / CompressionDecoder).
use crate::{
    Error, Result,
    buf::{Reader, write_varint},
};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use std::cell::RefCell;

/// Frames are at most three VarInt bytes long (21 bits).
pub const MAX_FRAME: usize = (1 << 21) - 1;
/// CompressionDecoder.MAXIMUM_UNCOMPRESSED_LENGTH.
pub const MAX_UNCOMPRESSED: usize = 8_388_608;
/// How much room a stream that still has output to give gets, once the first
/// guess is used up (it doubles, so a packet rarely asks more than twice).
const ZLIB_STEP: usize = 32 * 1024;

thread_local! {
    /// The zlib streams this thread compresses and decompresses with, kept for
    /// the thread's life so a packet pays for its own bytes and not for a codec
    /// allocation. Each use starts with a reset, which drops the previous
    /// packet's stream state, so one packet never leaks into the next.
    static CODECS: RefCell<Codecs> = RefCell::new(Codecs::new());
}

struct Codecs {
    /// Zlib wrapper (not raw deflate), default level, like `ZlibEncoder`.
    deflate: Compress,
    /// Zlib wrapper, like `ZlibDecoder`.
    inflate: Decompress,
}

impl Codecs {
    fn new() -> Self {
        Self {
            deflate: Compress::new(Compression::default(), true),
            inflate: Decompress::new(true),
        }
    }
}

/// Run `f` with this thread's codecs. A call made from inside a call on the
/// same thread (a panic hook, say) gets its own codec instead of panicking on
/// the borrow; nothing can be retained past the call either way.
fn with_codecs<T>(f: impl FnOnce(&mut Codecs) -> Result<T>) -> Result<T> {
    CODECS.with(|cell| match cell.try_borrow_mut() {
        Ok(mut codecs) => f(&mut codecs),
        Err(_) => f(&mut Codecs::new()),
    })
}

/// Build the frame for one packet body (`id` + payload already concatenated):
/// length prefix, and with compression enabled the uncompressed-length
/// field (0 when below the threshold).
pub fn encode(body: &[u8], threshold: Option<usize>) -> Result<Vec<u8>> {
    Ok(prefixed(&contents(body, threshold)?))
}

/// What [`encode`] puts after the length prefix: the same for every
/// connection with the same threshold.
pub fn contents(body: &[u8], threshold: Option<usize>) -> Result<Vec<u8>> {
    let inner = match threshold {
        None => body.to_vec(),
        Some(t) if body.len() < t => {
            let mut v = Vec::with_capacity(body.len() + 1);
            v.push(0);
            v.extend_from_slice(body);
            v
        }
        Some(_) => {
            // Room for the uncompressed size, then the stream. A compressible
            // body needs far less than this; one that is not grows below.
            let mut v = Vec::with_capacity(body.len() / 2 + 8);
            write_varint(&mut v, body.len() as i32);
            with_codecs(|codecs| deflate_into(&mut codecs.deflate, body, &mut v))?;
            v
        }
    };
    if inner.len() > MAX_FRAME {
        return Err(Error::new(format!(
            "packet too large: {} bytes",
            inner.len()
        )));
    }
    Ok(inner)
}

/// Append `body` to `out` as one complete zlib stream.
fn deflate_into(deflate: &mut Compress, body: &[u8], out: &mut Vec<u8>) -> Result<()> {
    deflate.reset();
    let mut fed = 0;
    loop {
        // `compress_vec` writes into the spare capacity and never grows it.
        if out.capacity() == out.len() {
            out.reserve(ZLIB_STEP.max(out.capacity()));
        }
        let before_in = deflate.total_in();
        let before_out = deflate.total_out();
        let status = deflate
            .compress_vec(&body[fed..], out, FlushCompress::Finish)
            .map_err(std::io::Error::from)?;
        fed += (deflate.total_in() - before_in) as usize;
        match status {
            Status::StreamEnd => return Ok(()),
            // `Ok` and `BufError` both mean the stream is not finished yet:
            // more room (or another call) is needed, which the loop gives it.
            Status::Ok | Status::BufError => {}
        }
        if deflate.total_in() == before_in && deflate.total_out() == before_out {
            // Room was made and nothing moved: no amount of retrying finishes.
            return Err(Error::new("deflate compression error: insufficient space"));
        }
    }
}

/// A frame: the length prefix, then the [`contents`].
pub fn prefixed(contents: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 3);
    write_varint(&mut out, contents.len() as i32);
    out.extend_from_slice(contents);
    out
}

/// Split one complete frame off the front of `buffer`, if available.
/// Returns the frame contents (without the length prefix).
pub fn split(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    let mut len = 0usize;
    for i in 0..3 {
        let Some(&b) = buffer.get(i) else {
            return Ok(None);
        };
        len |= usize::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            if len == 0 {
                // Varint21FrameDecoder: a frame with no bytes is a
                // corrupted frame, not an empty packet.
                return Err(Error::new("Frame length cannot be zero"));
            }
            let start = i + 1;
            if buffer.len() < start + len {
                return Ok(None);
            }
            let frame = buffer[start..start + len].to_vec();
            buffer.drain(..start + len);
            return Ok(Some(frame));
        }
    }
    Err(Error::new("length wider than 21 bits"))
}

/// Undo compression on a frame, giving the packet body (id + payload).
pub fn decompress(frame: Vec<u8>, threshold: Option<usize>) -> Result<Vec<u8>> {
    let Some(threshold) = threshold else {
        return Ok(frame);
    };
    let mut r = Reader::new(&frame);
    let size = r.varint()?;
    if size == 0 {
        return Ok(frame[r.position()..].to_vec());
    }
    let size = usize::try_from(size).map_err(|_| Error::new("negative uncompressed length"))?;
    if size < threshold {
        return Err(Error::new(format!(
            "Badly compressed packet - size of {size} is below server threshold of {threshold}"
        )));
    }
    if size > MAX_UNCOMPRESSED {
        return Err(Error::new(format!(
            "Badly compressed packet - size of {size} is larger than protocol maximum"
        )));
    }
    let start = r.position();
    with_codecs(|codecs| inflate_from(&mut codecs.inflate, &frame[start..], size))
}

/// `size` bytes of zlib stream: exactly `size` of them, or an error.
fn inflate_from(inflate: &mut Decompress, input: &[u8], size: usize) -> Result<Vec<u8>> {
    inflate.reset(true);
    // `size + 1` bytes is all the room a stream gets: one byte past the
    // declared size already says the declared size is wrong, so a stream that
    // keeps producing must not be inflated in full.
    let mut out = Vec::with_capacity(size + 1);
    let mut fed = 0;
    let mut ended = false;
    while out.len() <= size && out.capacity() > out.len() {
        let before_in = inflate.total_in();
        let before_out = inflate.total_out();
        let status = inflate
            .decompress_vec(&input[fed..], &mut out, FlushDecompress::Finish)
            .map_err(std::io::Error::from)?;
        fed += (inflate.total_in() - before_in) as usize;
        if status == Status::StreamEnd {
            ended = true;
            break;
        }
        if inflate.total_in() == before_in && inflate.total_out() == before_out {
            // No room left, or the input ran out mid-stream.
            break;
        }
    }
    if out.len() > size {
        return Err(Error::new(format!(
            "Badly compressed packet - actual length {} does not match declared size {size}",
            size + 1
        )));
    }
    if !ended {
        // Whatever came in was not one complete stream: never accept a
        // partial one, even when it happens to produce the declared length.
        return Err(Error::new(
            "deflate decompression error: stream did not terminate",
        ));
    }
    if out.len() != size {
        return Err(Error::new(format!(
            "Badly compressed packet - actual length {} does not match declared size {size}",
            out.len()
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    /// Compress `body` the way a peer that does not reuse a codec would.
    fn zlib(body: &[u8]) -> Vec<u8> {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(body).unwrap();
        z.finish().unwrap()
    }

    /// `declared`, then `stream`, as a frame's contents.
    fn compressed_frame(declared: i32, stream: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        write_varint(&mut frame, declared);
        frame.extend_from_slice(stream);
        frame
    }

    /// `len` deterministic bytes that do not compress, so a packet's stream
    /// has to be given room to grow.
    fn synthetic(len: usize, salt: u64) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1D ^ salt;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    /// Frame `body` and read it back; all callers pass a body at or above the
    /// threshold, so the declared size proves it went through the stream.
    fn round_trip(body: &[u8], threshold: usize) -> Vec<u8> {
        assert!(body.len() >= threshold);
        let mut buffer = encode(body, Some(threshold)).unwrap();
        let frame = split(&mut buffer).unwrap().unwrap();
        assert!(buffer.is_empty());
        assert_eq!(Reader::new(&frame).varint().unwrap(), body.len() as i32);
        decompress(frame, Some(threshold)).unwrap()
    }

    #[test]
    fn uncompressed_frames_round_trip() {
        let body = vec![0x00, 1, 2, 3];
        let framed = encode(&body, None).unwrap();
        assert_eq!(framed, [4, 0, 1, 2, 3]);
        let mut buffer = framed.clone();
        buffer.extend_from_slice(&framed[..2]);
        assert_eq!(split(&mut buffer).unwrap(), Some(body));
        assert_eq!(split(&mut buffer).unwrap(), None);
        assert_eq!(buffer, [4, 0]);
    }

    #[test]
    fn compression_threshold_like_vanilla() {
        // Below the threshold: a zero data length, then the raw body.
        let small = vec![7u8; 10];
        let framed = encode(&small, Some(256)).unwrap();
        assert_eq!(&framed[..2], &[11, 0]);
        let mut buffer = framed;
        let frame = split(&mut buffer).unwrap().unwrap();
        assert_eq!(decompress(frame, Some(256)).unwrap(), small);
        // At or above: the uncompressed size and a zlib stream.
        let big: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
        let framed = encode(&big, Some(256)).unwrap();
        assert!(framed.len() < big.len());
        let mut buffer = framed;
        let frame = split(&mut buffer).unwrap().unwrap();
        assert_eq!(Reader::new(&frame).varint().unwrap(), 5000);
        assert_eq!(decompress(frame, Some(256)).unwrap(), big);
    }

    #[test]
    fn rejects_bad_compressed_lengths() {
        // Declared size below the threshold is a protocol error.
        assert!(decompress(compressed_frame(10, &zlib(&[1; 10])), Some(256)).is_err());
        // Declared size that does not match the stream.
        assert!(decompress(compressed_frame(300, &zlib(&[1; 299])), Some(256)).is_err());
    }

    #[test]
    fn a_noncanonical_length_prefix_still_reads() {
        // The data-length VarInt is read, not measured: zero as `0x80 0x00`.
        assert_eq!(
            decompress(vec![0x80, 0x00, 7, 1, 2], Some(256)).unwrap(),
            [7, 1, 2]
        );
        // And a compressed packet whose size was written wide: 300.
        let body = vec![9; 300];
        let mut frame = vec![0xAC, 0x82, 0x00];
        frame.extend_from_slice(&zlib(&body));
        assert_eq!(decompress(frame, Some(256)).unwrap(), body);
    }

    #[test]
    fn reusing_the_codecs_round_trips_many_packets() {
        // Streams reused per thread: every packet has to stand alone, whatever
        // the one before it was.
        let mut bodies = vec![
            vec![0u8; 300], // above the threshold, all zeroes
            vec![1u8; 256], // exactly the threshold
            vec![2u8; 4096],
            synthetic(1 << 20, 1), // 1 MiB, not compressible
        ];
        for i in 0..120u32 {
            let len = 200 + (i as usize * 977) % 20_000;
            bodies.push(synthetic(len, u64::from(i) + 2));
        }
        for (i, body) in bodies.iter().enumerate() {
            assert_eq!(&round_trip(body, 128)[..], &body[..], "packet {i}");
        }
        // Twice through the same bodies: the second pass must be identical to
        // the first, stream state having been reset in between.
        for (i, body) in bodies.iter().enumerate() {
            assert_eq!(&round_trip(body, 128)[..], &body[..], "packet {i} again");
        }
    }

    #[test]
    fn a_bad_stream_after_a_good_one_still_errors() {
        let good = vec![3u8; 500];
        assert_eq!(round_trip(&good, 256), good);

        // Truncated: the declared size is produced, but the stream never ends.
        let stream = zlib(&good);
        let truncated = compressed_frame(500, &stream[..stream.len() - 4]);
        assert!(decompress(truncated, Some(256)).is_err());

        // Corrupt: not a zlib stream at all.
        assert!(decompress(compressed_frame(500, &[0xFF; 64]), Some(256)).is_err());

        // A stream that keeps going past the declared size.
        let longer = compressed_frame(256, &zlib(&vec![9u8; 4000]));
        let err = decompress(longer, Some(256)).unwrap_err();
        assert_eq!(
            err.0,
            "Badly compressed packet - actual length 257 does not match declared size 256"
        );

        // And the codec still works after each of those.
        assert_eq!(round_trip(&good, 256), good);
        let other = synthetic(9000, 7);
        assert_eq!(round_trip(&other, 256), other);
    }

    #[test]
    fn decompressed_size_mismatch_reports_the_actual_length() {
        // Fewer bytes than declared.
        let err = decompress(compressed_frame(300, &zlib(&[1; 299])), Some(256)).unwrap_err();
        assert_eq!(
            err.0,
            "Badly compressed packet - actual length 299 does not match declared size 300"
        );
    }
}
