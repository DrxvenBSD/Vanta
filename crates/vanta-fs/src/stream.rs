//! Streaming decoded file input.
//!
//! Small files are decoded with [`crate::decode_file`]. Large files use
//! [`open_decoded`] so the rope is built from bounded chunks instead of a
//! whole-file byte buffer plus a second decoded string. Detection and
//! validation still read the file in bounded chunks; the file is then reopened
//! at the data offset and decoded into UTF-8 on demand.
//!
//! The returned [`DecodedReader`] never yields an incomplete UTF-8 sequence.
//! Its line-ending detection belongs to the text layer, which sees the
//! decoded stream.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::TextEncoding;

/// The byte size of each streaming read.
const CHUNK: usize = 64 * 1024;

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];
const UTF32_LE_BOM: [u8; 4] = [0xFF, 0xFE, 0x00, 0x00];
const UTF32_BE_BOM: [u8; 4] = [0x00, 0x00, 0xFE, 0xFF];

/// An open, decoded file stream and its encoding metadata.
#[derive(Debug)]
pub struct DecodedFile {
    /// The UTF-8 stream, positioned after any preserved BOM.
    pub reader: DecodedReader,
    /// The encoding detected for the file.
    pub encoding: TextEncoding,
    /// Whether the source has a UTF-8 BOM to preserve on write.
    pub bom: bool,
    /// A non-fatal notice when malformed data fell back to latin-1.
    pub warning: Option<String>,
}

/// A bounded, decoding UTF-8 reader over a file.
///
/// The reader owns a fixed-size scratch area and retains at most one partial
/// code unit for UTF-16/32. It does not hold the whole source or decoded text.
#[derive(Debug)]
pub struct DecodedReader {
    inner: BufReader<File>,
    encoding: TextEncoding,
    carry: Vec<u8>,
    high_surrogate: Option<u16>,
    pending: Vec<u8>,
    pending_start: usize,
    scratch: Vec<u8>,
}

impl DecodedReader {
    fn new(inner: BufReader<File>, encoding: TextEncoding) -> Self {
        Self {
            inner,
            encoding,
            carry: Vec::with_capacity(4),
            high_surrogate: None,
            pending: Vec::new(),
            pending_start: 0,
            scratch: Vec::with_capacity(CHUNK),
        }
    }

    /// Fills `pending` with the next decoded chunk. Returns `false` at EOF.
    fn fill_pending(&mut self) -> io::Result<bool> {
        let mut buffer = [0u8; CHUNK];
        let count = self.inner.read(&mut buffer)?;
        if count == 0 {
            if !self.carry.is_empty() || self.high_surrogate.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "encoded input ends with an incomplete code unit",
                ));
            }
            return Ok(false);
        }

        self.scratch.clear();
        match self.encoding {
            TextEncoding::Utf8 => self.scratch.extend_from_slice(&buffer[..count]),
            TextEncoding::Latin1 => {
                for &byte in &buffer[..count] {
                    if byte < 0x80 {
                        self.scratch.push(byte);
                    } else {
                        self.scratch.push(0xC0 | (byte >> 6));
                        self.scratch.push(0x80 | (byte & 0x3F));
                    }
                }
            }
            TextEncoding::Utf16Le => self.decode_utf16(&buffer[..count], true)?,
            TextEncoding::Utf16Be => self.decode_utf16(&buffer[..count], false)?,
            TextEncoding::Utf32Le => self.decode_utf32(&buffer[..count], true)?,
            TextEncoding::Utf32Be => self.decode_utf32(&buffer[..count], false)?,
        }

        if self.scratch.is_empty() {
            // A chunk can contain only a partial unit. Ask the caller to read
            // the next chunk instead of reporting a short read as EOF.
            return Ok(true);
        }

        std::mem::swap(&mut self.pending, &mut self.scratch);
        self.pending_start = 0;
        Ok(true)
    }

    /// Decodes UTF-16 code units, retaining a high surrogate across chunks.
    fn decode_utf16(&mut self, bytes: &[u8], little_endian: bool) -> io::Result<()> {
        let mut data = Vec::with_capacity(self.carry.len() + bytes.len());
        data.extend_from_slice(&self.carry);
        data.extend_from_slice(bytes);

        let mut index = 0;
        while index + 1 < data.len() {
            let unit = if little_endian {
                u16::from_le_bytes([data[index], data[index + 1]])
            } else {
                u16::from_be_bytes([data[index], data[index + 1]])
            };
            index += 2;

            if let Some(high) = self.high_surrogate.take() {
                if !(0xDC00..=0xDFFF).contains(&unit) {
                    return Err(invalid_data("unpaired UTF-16 high surrogate"));
                }
                let scalar =
                    0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                self.push_scalar(scalar)?;
            } else if (0xD800..=0xDBFF).contains(&unit) {
                self.high_surrogate = Some(unit);
            } else if (0xDC00..=0xDFFF).contains(&unit) {
                return Err(invalid_data(
                    "UTF-16 low surrogate without a high surrogate",
                ));
            } else {
                self.push_scalar(u32::from(unit))?;
            }
        }

        self.carry.clear();
        self.carry.extend_from_slice(&data[index..]);
        Ok(())
    }

    /// Decodes UTF-32 code points, retaining partial units across chunks.
    fn decode_utf32(&mut self, bytes: &[u8], little_endian: bool) -> io::Result<()> {
        let mut data = Vec::with_capacity(self.carry.len() + bytes.len());
        data.extend_from_slice(&self.carry);
        data.extend_from_slice(bytes);

        let mut index = 0;
        while index + 3 < data.len() {
            let value = if little_endian {
                u32::from_le_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ])
            } else {
                u32::from_be_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ])
            };
            index += 4;
            self.push_scalar(value)?;
        }

        self.carry.clear();
        self.carry.extend_from_slice(&data[index..]);
        Ok(())
    }

    /// Encodes one scalar into the scratch buffer.
    fn push_scalar(&mut self, value: u32) -> io::Result<()> {
        let character =
            char::from_u32(value).ok_or_else(|| invalid_data("invalid Unicode scalar value"))?;
        let mut encoded = [0u8; 4];
        self.scratch
            .extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        Ok(())
    }

    /// Copies pending decoded bytes into `output`.
    fn copy_pending(&mut self, output: &mut [u8]) -> usize {
        let available = self.pending.len() - self.pending_start;
        let count = available.min(output.len());
        output[..count]
            .copy_from_slice(&self.pending[self.pending_start..self.pending_start + count]);
        self.pending_start += count;
        if self.pending_start == self.pending.len() {
            self.pending.clear();
            self.pending_start = 0;
        }
        count
    }
}

impl Read for DecodedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        if self.encoding == TextEncoding::Utf8 && self.pending.is_empty() {
            return self.inner.read(output);
        }

        loop {
            if self.pending_start < self.pending.len() {
                return Ok(self.copy_pending(output));
            }
            if !self.fill_pending()? {
                return Ok(0);
            }
        }
    }
}

/// Opens `path` and returns a bounded UTF-8 decoding stream.
///
/// Detection is BOM-first and validation scans the source in chunks. Valid
/// UTF-8, UTF-16LE/BE and UTF-32LE/BE with a BOM, and latin-1 are supported.
/// Malformed BOM-prefixed data falls back to a byte-preserving latin-1 read
/// with a warning, matching [`crate::decode_file`].
///
/// # Errors
///
/// Returns the underlying I/O error when the file cannot be opened, read or
/// seeked. Encoding failures do not abort the open; they become a latin-1
/// fallback with a warning.
pub fn open_decoded(path: &Path) -> io::Result<DecodedFile> {
    let (candidate, bom_len) = detect_bom(path)?;
    let valid = match candidate {
        TextEncoding::Utf8 => validate_utf8(path, bom_len)?,
        TextEncoding::Utf16Le => validate_utf16(path, bom_len, true)?,
        TextEncoding::Utf16Be => validate_utf16(path, bom_len, false)?,
        TextEncoding::Utf32Le => validate_utf32(path, bom_len, true)?,
        TextEncoding::Utf32Be => validate_utf32(path, bom_len, false)?,
        TextEncoding::Latin1 => true,
    };

    let (encoding, bom, warning, skip) = if valid {
        (candidate, bom_len != 0, None, bom_len)
    } else {
        (
            TextEncoding::Latin1,
            false,
            Some(format!(
                "invalid {} data; opened losslessly as latin1",
                candidate.name()
            )),
            0,
        )
    };

    let mut file = File::open(path)?;
    if skip != 0 {
        file.seek(SeekFrom::Start(skip as u64))?;
    }
    let inner = BufReader::with_capacity(CHUNK, file);
    Ok(DecodedFile {
        reader: DecodedReader::new(inner, encoding),
        encoding,
        bom,
        warning,
    })
}

/// Reads the first four bytes and returns the BOM-detected encoding and data
/// offset.
fn detect_bom(path: &Path) -> io::Result<(TextEncoding, usize)> {
    let mut file = File::open(path)?;
    let mut prefix = [0u8; 4];
    let mut count = 0;
    while count < prefix.len() {
        let read = file.read(&mut prefix[count..])?;
        if read == 0 {
            break;
        }
        count += read;
    }
    let bytes = &prefix[..count];

    if bytes.starts_with(&UTF8_BOM) {
        Ok((TextEncoding::Utf8, UTF8_BOM.len()))
    } else if bytes.starts_with(&UTF32_LE_BOM) {
        Ok((TextEncoding::Utf32Le, UTF32_LE_BOM.len()))
    } else if bytes.starts_with(&UTF32_BE_BOM) {
        Ok((TextEncoding::Utf32Be, UTF32_BE_BOM.len()))
    } else if bytes.starts_with(&UTF16_LE_BOM) {
        Ok((TextEncoding::Utf16Le, UTF16_LE_BOM.len()))
    } else if bytes.starts_with(&UTF16_BE_BOM) {
        Ok((TextEncoding::Utf16Be, UTF16_BE_BOM.len()))
    } else {
        Ok((TextEncoding::Utf8, 0))
    }
}

/// Opens a reader positioned after `skip` bytes.
fn reader_at(path: &Path, skip: usize) -> io::Result<BufReader<File>> {
    let mut file = File::open(path)?;
    if skip != 0 {
        file.seek(SeekFrom::Start(skip as u64))?;
    }
    Ok(BufReader::with_capacity(CHUNK, file))
}

/// Validates that the source after `skip` is complete, valid UTF-8.
fn validate_utf8(path: &Path, skip: usize) -> io::Result<bool> {
    let mut reader = reader_at(path, skip)?;
    let mut buffer = [0u8; CHUNK];
    let mut carry = Vec::new();
    let mut data = Vec::with_capacity(CHUNK + 4);

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(carry.is_empty());
        }

        data.clear();
        data.extend_from_slice(&carry);
        data.extend_from_slice(&buffer[..count]);
        match std::str::from_utf8(&data) {
            Ok(_) => carry.clear(),
            Err(error) => {
                if error.error_len().is_some() {
                    return Ok(false);
                }
                let valid_up_to = error.valid_up_to();
                carry.clear();
                carry.extend_from_slice(&data[valid_up_to..]);
            }
        }
    }
}

/// Validates UTF-16 code units after `skip`, including surrogate pairing.
fn validate_utf16(path: &Path, skip: usize, little_endian: bool) -> io::Result<bool> {
    let mut reader = reader_at(path, skip)?;
    let mut buffer = [0u8; CHUNK];
    let mut carry = Vec::new();
    let mut data = Vec::with_capacity(CHUNK + 2);
    let mut high_surrogate = false;

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(carry.is_empty() && !high_surrogate);
        }

        data.clear();
        data.extend_from_slice(&carry);
        data.extend_from_slice(&buffer[..count]);
        let mut index = 0;
        while index + 1 < data.len() {
            let unit = if little_endian {
                u16::from_le_bytes([data[index], data[index + 1]])
            } else {
                u16::from_be_bytes([data[index], data[index + 1]])
            };
            index += 2;

            if high_surrogate {
                if !(0xDC00..=0xDFFF).contains(&unit) {
                    return Ok(false);
                }
                high_surrogate = false;
            } else if (0xD800..=0xDBFF).contains(&unit) {
                high_surrogate = true;
            } else if (0xDC00..=0xDFFF).contains(&unit) {
                return Ok(false);
            }
        }

        carry.clear();
        carry.extend_from_slice(&data[index..]);
    }
}

/// Validates UTF-32 code points after `skip`.
fn validate_utf32(path: &Path, skip: usize, little_endian: bool) -> io::Result<bool> {
    let mut reader = reader_at(path, skip)?;
    let mut buffer = [0u8; CHUNK];
    let mut carry = Vec::new();
    let mut data = Vec::with_capacity(CHUNK + 4);

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(carry.is_empty());
        }

        data.clear();
        data.extend_from_slice(&carry);
        data.extend_from_slice(&buffer[..count]);
        let mut index = 0;
        while index + 3 < data.len() {
            let value = if little_endian {
                u32::from_le_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ])
            } else {
                u32::from_be_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ])
            };
            index += 4;
            if char::from_u32(value).is_none() {
                return Ok(false);
            }
        }

        carry.clear();
        carry.extend_from_slice(&data[index..]);
    }
}

/// Creates an `InvalidData` error for a malformed code unit.
fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
