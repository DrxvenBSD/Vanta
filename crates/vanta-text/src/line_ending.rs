//! Line-ending metadata.
//!
//! The buffer preserves bytes exactly; it never rewrites CRLF to LF on load
//! (AGENTS §10.1). Line endings are reported so the file layer can re-encode
//! on write (BUF-007) and the statusline can display `fileformat`.

/// The line-ending style detected for a buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// Lines end with a single `\n`.
    #[default]
    Lf,
    /// Lines end with `\r\n`.
    Crlf,
}

impl LineEnding {
    /// Returns the byte sequence written for this line ending.
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Crlf => b"\r\n",
        }
    }

    /// Detects the line ending of `text` from its first line terminator.
    ///
    /// Text without a terminator reports [`LineEnding::Lf`], matching Vim's
    /// default `fileformat` for new buffers. Files with mixed endings report
    /// the style of the first line; mixed endings are preserved on write.
    pub fn detect(text: &str) -> Self {
        let bytes = text.as_bytes();
        match bytes.iter().position(|&byte| byte == b'\n') {
            Some(index) if index > 0 && bytes[index - 1] == b'\r' => Self::Crlf,
            _ => Self::Lf,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_endings() {
        assert_eq!(LineEnding::detect(""), LineEnding::Lf);
        assert_eq!(LineEnding::detect("one line"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("\r\n"), LineEnding::Crlf);
    }

    #[test]
    fn reports_terminator_bytes() {
        assert_eq!(LineEnding::Lf.as_bytes(), b"\n");
        assert_eq!(LineEnding::Crlf.as_bytes(), b"\r\n");
    }
}
