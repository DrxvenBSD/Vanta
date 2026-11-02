//! Text encoding detection and conversion for file reads and writes.
//!
//! `PROJECT_SPEC` BUF-007 requires UTF-8, UTF-16 and UTF-32 with a BOM, and
//! latin-1 to round-trip without silent loss. Detection is BOM-first; a file
//! without a BOM is UTF-8 when valid and latin-1 otherwise. The latin-1
//! fallback is byte-preserving: every input byte maps to the same Unicode
//! scalar in `U+0000..=U+00FF`, so an unchanged file writes back identical
//! bytes.
//!
//! Malformed BOM-prefixed data is not partially decoded. It falls back to
//! latin-1 with a warning, which also preserves every byte; the caller must
//! not silently write a different encoding.

use std::fmt;

/// The UTF-8 byte-order mark.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// The UTF-16 little-endian byte-order mark.
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
/// The UTF-16 big-endian byte-order mark.
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];
/// The UTF-32 little-endian byte-order mark.
const UTF32_LE_BOM: [u8; 4] = [0xFF, 0xFE, 0x00, 0x00];
/// The UTF-32 big-endian byte-order mark.
const UTF32_BE_BOM: [u8; 4] = [0x00, 0x00, 0xFE, 0xFF];

/// A text encoding supported for file input and output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextEncoding {
    /// UTF-8, optionally with a byte-order mark.
    Utf8,
    /// UTF-16 with a little-endian byte order and a mandatory BOM on write.
    Utf16Le,
    /// UTF-16 with a big-endian byte order and a mandatory BOM on write.
    Utf16Be,
    /// UTF-32 with a little-endian byte order and a mandatory BOM on write.
    Utf32Le,
    /// UTF-32 with a big-endian byte order and a mandatory BOM on write.
    Utf32Be,
    /// ISO-8859-1, which represents `U+0000..=U+00FF` as one byte each.
    Latin1,
}

impl TextEncoding {
    /// Returns the canonical Vim-style name of this encoding.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Utf32Le => "utf-32le",
            Self::Utf32Be => "utf-32be",
            Self::Latin1 => "latin1",
        }
    }

    /// Parses a `fileencoding` value.
    ///
    /// The accepted aliases are the common Vim spellings. A bare `utf-16` or
    /// `utf-32` selects the little-endian form. Returns `None` for an unknown
    /// name; callers report `E474` rather than guessing.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" => Some(Self::Utf8),
            "utf-16" | "utf-16le" | "utf16le" | "ucs-2le" => Some(Self::Utf16Le),
            "utf-16be" | "utf16be" | "ucs-2be" => Some(Self::Utf16Be),
            "utf-32" | "utf-32le" | "utf32le" | "ucs-4le" => Some(Self::Utf32Le),
            "utf-32be" | "utf32be" | "ucs-4be" => Some(Self::Utf32Be),
            "latin1" | "latin-1" | "iso-8859-1" | "iso8859-1" => Some(Self::Latin1),
            _ => None,
        }
    }
}

impl fmt::Display for TextEncoding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The result of decoding a file's bytes into the editor's UTF-8 text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedText {
    /// The decoded text, always valid UTF-8.
    pub text: String,
    /// The encoding to use when writing the buffer back.
    pub encoding: TextEncoding,
    /// Whether the source had a UTF-8 BOM that should be preserved.
    ///
    /// UTF-16 and UTF-32 always write their mandatory BOM, so this is only
    /// meaningful for [`TextEncoding::Utf8`].
    pub bom: bool,
    /// A non-fatal notice explaining a lossless latin-1 fallback.
    pub warning: Option<String>,
}

/// A character that the selected encoding cannot represent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The buffer text contains a scalar outside the encoding's repertoire.
    #[error("E513: cannot encode U+{codepoint:04X} as {encoding} at byte {offset}")]
    Unrepresentable {
        /// The encoding requested for the write.
        encoding: TextEncoding,
        /// The Unicode scalar value that cannot be represented.
        codepoint: u32,
        /// The UTF-8 byte offset of the character in the buffer.
        offset: usize,
    },
}

/// Decodes file bytes with BOM-first detection and a lossless latin-1
/// fallback.
///
/// Valid UTF-8 without a BOM is UTF-8. A UTF-8 BOM is stripped from the text
/// and preserved for the next write. UTF-16 and UTF-32 require their BOM.
/// Anything else, including malformed BOM-prefixed data, is decoded as latin-1
/// with a warning; no input byte is discarded.
pub fn decode_file(bytes: &[u8]) -> DecodedText {
    if let Some(rest) = bytes.strip_prefix(&UTF8_BOM) {
        return match std::str::from_utf8(rest) {
            Ok(text) => DecodedText {
                text: text.to_string(),
                encoding: TextEncoding::Utf8,
                bom: true,
                warning: None,
            },
            Err(error) => latin1_fallback(
                bytes,
                format!(
                    "invalid UTF-8 at byte {} after the BOM; opened losslessly as latin1",
                    error.valid_up_to() + UTF8_BOM.len()
                ),
            ),
        };
    }

    if let Some(rest) = bytes.strip_prefix(&UTF32_LE_BOM) {
        return match decode_utf32(rest, true) {
            Some(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf32Le,
                bom: true,
                warning: None,
            },
            None => latin1_fallback(
                bytes,
                "invalid UTF-32LE data; opened losslessly as latin1".to_string(),
            ),
        };
    }

    if let Some(rest) = bytes.strip_prefix(&UTF32_BE_BOM) {
        return match decode_utf32(rest, false) {
            Some(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf32Be,
                bom: true,
                warning: None,
            },
            None => latin1_fallback(
                bytes,
                "invalid UTF-32BE data; opened losslessly as latin1".to_string(),
            ),
        };
    }

    if let Some(rest) = bytes.strip_prefix(&UTF16_LE_BOM) {
        return match decode_utf16(rest, true) {
            Some(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf16Le,
                bom: true,
                warning: None,
            },
            None => latin1_fallback(
                bytes,
                "invalid UTF-16LE data; opened losslessly as latin1".to_string(),
            ),
        };
    }

    if let Some(rest) = bytes.strip_prefix(&UTF16_BE_BOM) {
        return match decode_utf16(rest, false) {
            Some(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf16Be,
                bom: true,
                warning: None,
            },
            None => latin1_fallback(
                bytes,
                "invalid UTF-16BE data; opened losslessly as latin1".to_string(),
            ),
        };
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => DecodedText {
            text: text.to_string(),
            encoding: TextEncoding::Utf8,
            bom: false,
            warning: None,
        },
        Err(error) => latin1_fallback(
            bytes,
            format!(
                "invalid UTF-8 at byte {}; opened losslessly as latin1",
                error.valid_up_to()
            ),
        ),
    }
}

/// Encodes buffer text for a write.
///
/// UTF-16 and UTF-32 always include their BOM. UTF-8 includes a BOM only when
/// `bomb` is true. Latin-1 fails with the offending byte offset instead of
/// replacing or dropping a character.
///
/// # Errors
///
/// Returns [`EncodeError::Unrepresentable`] when latin-1 cannot represent a
/// scalar in `text`.
pub fn encode_text(text: &str, encoding: TextEncoding, bomb: bool) -> Result<Vec<u8>, EncodeError> {
    match encoding {
        TextEncoding::Utf8 => {
            let mut output = Vec::with_capacity(text.len() + UTF8_BOM.len());
            if bomb {
                output.extend_from_slice(&UTF8_BOM);
            }
            output.extend_from_slice(text.as_bytes());
            Ok(output)
        }
        TextEncoding::Utf16Le => Ok(encode_utf16(text, true)),
        TextEncoding::Utf16Be => Ok(encode_utf16(text, false)),
        TextEncoding::Utf32Le => Ok(encode_utf32(text, true)),
        TextEncoding::Utf32Be => Ok(encode_utf32(text, false)),
        TextEncoding::Latin1 => encode_latin1(text),
    }
}

/// Decodes bytes after a UTF-16 BOM.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }

    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        });
    }

    let mut text = String::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if (0xD800..=0xDBFF).contains(&unit) {
            let low = *units.get(index + 1)?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return None;
            }
            let scalar = 0x1_0000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
            text.push(char::from_u32(scalar)?);
            index += 2;
        } else if (0xDC00..=0xDFFF).contains(&unit) {
            return None;
        } else {
            text.push(char::from_u32(u32::from(unit))?);
            index += 1;
        }
    }
    Some(text)
}

/// Decodes bytes after a UTF-32 BOM.
fn decode_utf32(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }

    let mut text = String::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let value = if little_endian {
            u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        } else {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        };
        text.push(char::from_u32(value)?);
    }
    Some(text)
}

/// Encodes text as UTF-16 with a mandatory BOM.
fn encode_utf16(text: &str, little_endian: bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(2 * (text.len() + 1));
    output.extend_from_slice(if little_endian {
        &UTF16_LE_BOM
    } else {
        &UTF16_BE_BOM
    });
    for unit in text.encode_utf16() {
        let encoded = if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        output.extend_from_slice(&encoded);
    }
    output
}

/// Encodes text as UTF-32 with a mandatory BOM.
fn encode_utf32(text: &str, little_endian: bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(4 * (text.chars().count() + 1));
    output.extend_from_slice(if little_endian {
        &UTF32_LE_BOM
    } else {
        &UTF32_BE_BOM
    });
    for character in text.chars() {
        let value = character as u32;
        let encoded = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output.extend_from_slice(&encoded);
    }
    output
}

/// Encodes text as latin-1, failing on the first unrepresentable character.
fn encode_latin1(text: &str) -> Result<Vec<u8>, EncodeError> {
    let mut output = Vec::with_capacity(text.len());
    for (offset, character) in text.char_indices() {
        let codepoint = character as u32;
        if codepoint > 0xFF {
            return Err(EncodeError::Unrepresentable {
                encoding: TextEncoding::Latin1,
                codepoint,
                offset,
            });
        }
        output.push(codepoint as u8);
    }
    Ok(output)
}

/// Builds a latin-1 decoding that preserves the original bytes.
fn latin1_fallback(bytes: &[u8], warning: String) -> DecodedText {
    DecodedText {
        text: bytes.iter().map(|&byte| char::from(byte)).collect(),
        encoding: TextEncoding::Latin1,
        bom: false,
        warning: Some(warning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_fileencoding_names() {
        assert_eq!(TextEncoding::parse("UTF-8"), Some(TextEncoding::Utf8));
        assert_eq!(TextEncoding::parse("utf8"), Some(TextEncoding::Utf8));
        assert_eq!(TextEncoding::parse("utf-16"), Some(TextEncoding::Utf16Le));
        assert_eq!(TextEncoding::parse("utf-16be"), Some(TextEncoding::Utf16Be));
        assert_eq!(TextEncoding::parse("utf-32"), Some(TextEncoding::Utf32Le));
        assert_eq!(
            TextEncoding::parse("iso-8859-1"),
            Some(TextEncoding::Latin1)
        );
        assert_eq!(TextEncoding::parse("utf-7"), None);
        assert_eq!(TextEncoding::Utf16Le.name(), "utf-16le");
    }

    #[test]
    fn plain_utf8_round_trips_without_a_bom() {
        let bytes = "café\n🙂\n".as_bytes();
        let decoded = decode_file(bytes);
        assert_eq!(decoded.text, "café\n🙂\n");
        assert_eq!(decoded.encoding, TextEncoding::Utf8);
        assert!(!decoded.bom);
        assert!(decoded.warning.is_none());
        assert_eq!(
            encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
            bytes
        );
    }

    #[test]
    fn utf8_bom_is_detected_stripped_and_preserved() {
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice("hello\n".as_bytes());

        let decoded = decode_file(&bytes);
        assert_eq!(decoded.text, "hello\n");
        assert_eq!(decoded.encoding, TextEncoding::Utf8);
        assert!(decoded.bom);
        assert!(decoded.warning.is_none());
        assert_eq!(
            encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
            bytes
        );
    }

    #[test]
    fn utf16_endianness_and_astral_characters_round_trip() {
        for encoding in [TextEncoding::Utf16Le, TextEncoding::Utf16Be] {
            let text = "a\u{1F600}\n";
            let bytes = encode_text(text, encoding, false).expect("encode");
            let decoded = decode_file(&bytes);
            assert_eq!(decoded.text, text);
            assert_eq!(decoded.encoding, encoding);
            assert!(decoded.bom);
            assert!(decoded.warning.is_none());
            assert_eq!(
                encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
                bytes
            );
        }
    }

    #[test]
    fn utf32_endianness_round_trips() {
        for encoding in [TextEncoding::Utf32Le, TextEncoding::Utf32Be] {
            let text = "a中🙂\n";
            let bytes = encode_text(text, encoding, false).expect("encode");
            let decoded = decode_file(&bytes);
            assert_eq!(decoded.text, text);
            assert_eq!(decoded.encoding, encoding);
            assert!(decoded.bom);
            assert!(decoded.warning.is_none());
            assert_eq!(
                encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
                bytes
            );
        }
    }

    #[test]
    fn latin1_fallback_preserves_every_byte() {
        let bytes: Vec<u8> = (0..=255).collect();
        let decoded = decode_file(&bytes);
        assert_eq!(decoded.encoding, TextEncoding::Latin1);
        assert!(!decoded.bom);
        assert!(decoded.warning.is_some());
        assert_eq!(
            encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
            bytes
        );
    }

    #[test]
    fn malformed_bom_data_falls_back_without_losing_bytes() {
        let cases = [
            vec![0xEF, 0xBB, 0xBF, 0xFF],
            vec![0xFF, 0xFE, 0x00],
            vec![0xFF, 0xFE, 0x00, 0xD8],
            vec![0x00, 0x00, 0xFE, 0xFF, 0x00, 0x11, 0x00, 0x00],
        ];
        for bytes in cases {
            let decoded = decode_file(&bytes);
            assert_eq!(decoded.encoding, TextEncoding::Latin1);
            assert!(!decoded.bom);
            assert!(decoded.warning.is_some());
            assert_eq!(
                encode_text(&decoded.text, decoded.encoding, decoded.bom).expect("encode"),
                bytes,
                "fallback must preserve the source bytes"
            );
        }
    }

    #[test]
    fn unrepresentable_latin1_character_reports_its_byte_offset() {
        let error = encode_text("a€b", TextEncoding::Latin1, false).expect_err("must fail");
        assert_eq!(
            error,
            EncodeError::Unrepresentable {
                encoding: TextEncoding::Latin1,
                codepoint: 0x20AC,
                offset: 1,
            }
        );
        assert!(error.to_string().contains("byte 1"));
    }

    #[test]
    fn empty_input_is_utf8() {
        let decoded = decode_file(&[]);
        assert_eq!(decoded.text, "");
        assert_eq!(decoded.encoding, TextEncoding::Utf8);
        assert!(!decoded.bom);
        assert!(decoded.warning.is_none());
        assert_eq!(
            encode_text("", decoded.encoding, false).expect("encode"),
            b""
        );
    }
}
