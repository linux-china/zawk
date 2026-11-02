//! Transparent decompression of input files.
//!
//! Compressed inputs are recognized by their magic bytes rather than their extension, so
//! `x.csv.gz`, an extension-less download, and an `s3://` object all behave the same way.
//! Supported formats: gzip, zstd, bzip2, xz and lz4 (frame format).
use std::io::{self, Cursor, Read};

/// The most bytes we check: the bzip2 header and the magic of its first block.
const MAGIC_LEN: usize = 10;

/// What follows the bzip2 header: the magic of a compressed block, or of the end of an empty
/// stream.
const BZIP2_BLOCK_MAGIC: [u8; 6] = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59];
const BZIP2_EOS_MAGIC: [u8; 6] = [0x17, 0x72, 0x45, 0x38, 0x50, 0x90];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Gzip,
    Zstd,
    Bzip2,
    Xz,
    Lz4,
}

pub fn detect(prefix: &[u8]) -> Option<Format> {
    // The third gzip byte is the compression method, and deflate (8) is the only one defined.
    if prefix.starts_with(&[0x1f, 0x8b, 0x08]) {
        Some(Format::Gzip)
    } else if prefix.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Format::Zstd)
    } else if prefix.len() >= MAGIC_LEN
        && prefix.starts_with(b"BZh")
        && (b'1'..=b'9').contains(&prefix[3])
        && (prefix[4..MAGIC_LEN] == BZIP2_BLOCK_MAGIC || prefix[4..MAGIC_LEN] == BZIP2_EOS_MAGIC)
    {
        Some(Format::Bzip2)
    } else if prefix.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Format::Xz)
    } else if prefix.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
        Some(Format::Lz4)
    } else {
        None
    }
}

/// Peeks at the start of `r` and, if it is compressed, wraps it in the matching decoder.
/// Uncompressed input is passed through unchanged (with the peeked bytes put back in front).
pub fn maybe_decompress<R: Read + Send + 'static>(
    name: &str,
    mut r: R,
) -> io::Result<Box<dyn Read + Send>> {
    let mut prefix = [0u8; MAGIC_LEN];
    let mut n = 0;
    while n < MAGIC_LEN {
        match r.read(&mut prefix[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let format = detect(&prefix[..n]);
    let r = Cursor::new(prefix[..n].to_vec()).chain(r);
    let decoder: Box<dyn Read + Send> = match format {
        None => return Ok(Box::new(r)),
        Some(Format::Gzip) => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Some(Format::Zstd) => Box::new(zstd::stream::read::Decoder::new(r)?),
        Some(Format::Bzip2) => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        Some(Format::Xz) => Box::new(liblzma::read::XzDecoder::new_multi_decoder(r)),
        Some(Format::Lz4) => Box::new(lz4_flex::frame::FrameDecoder::new(r)),
    };
    Ok(Box::new(Decoder {
        inner: decoder,
        name: name.to_string(),
        format: format.unwrap(),
    }))
}

/// Names decoding errors after the file. Truncated input surfaces from the decoders as
/// `UnexpectedEof`, which the record readers treat as a plain end of input, so it is turned
/// into `InvalidData` to make it fatal.
struct Decoder {
    inner: Box<dyn Read + Send>,
    name: String,
    format: Format,
}

impl Read for Decoder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(|e| {
            let kind = match e.kind() {
                io::ErrorKind::UnexpectedEof => io::ErrorKind::InvalidData,
                io::ErrorKind::Interrupted => return e,
                kind => kind,
            };
            io::Error::new(
                kind,
                format!(
                    "cannot decompress {:?} file `{}': {}",
                    self.format, self.name, e
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TEXT: &[u8] = b"a,b,c\n1,2,3\n4,5,6\n";

    fn roundtrip(compressed: Vec<u8>) {
        let mut out = Vec::new();
        maybe_decompress("test", Cursor::new(compressed))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, TEXT);
    }

    #[test]
    fn plain_passthrough() {
        roundtrip(TEXT.to_vec());
        let mut out = Vec::new();
        maybe_decompress("test", Cursor::new(b"ab".to_vec()))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"ab");
    }

    #[test]
    fn text_starting_with_a_short_magic_is_plain() {
        let texts: [&[u8]; 4] = [
            b"BZh9 hello world\n",
            b"BZh9",
            b"\x1f\x8b plain\n",
            b"\x1f\x8b",
        ];
        for text in texts {
            let mut out = Vec::new();
            maybe_decompress("test", Cursor::new(text.to_vec()))
                .unwrap()
                .read_to_end(&mut out)
                .unwrap();
            assert_eq!(out, text);
        }
    }

    #[test]
    fn empty_bzip2_stream() {
        let e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        let bz = e.finish().unwrap();
        assert_eq!(detect(&bz), Some(Format::Bzip2));
        let mut out = Vec::new();
        maybe_decompress("test", Cursor::new(bz))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn gzip() {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(TEXT).unwrap();
        roundtrip(e.finish().unwrap());
    }

    #[test]
    fn truncated_is_an_error() {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(TEXT).unwrap();
        let mut gz = e.finish().unwrap();
        gz.truncate(gz.len() / 2);
        let mut out = Vec::new();
        let err = maybe_decompress("t.gz", Cursor::new(gz))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn zstd() {
        roundtrip(zstd::stream::encode_all(TEXT, 3).unwrap());
    }

    #[test]
    fn bzip2() {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        e.write_all(TEXT).unwrap();
        roundtrip(e.finish().unwrap());
    }

    #[test]
    fn xz() {
        let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
        e.write_all(TEXT).unwrap();
        roundtrip(e.finish().unwrap());
    }

    #[test]
    fn lz4() {
        let mut e = lz4_flex::frame::FrameEncoder::new(Vec::new());
        e.write_all(TEXT).unwrap();
        roundtrip(e.finish().unwrap());
    }
}
