//! Checksummed persistent undo files.
//!
//! Undo files live under `<state>/undo`, keyed by the canonical target path.
//! The payload carries a text hash so a history is never attached to different
//! file contents. Files are bounded, versioned, atomically replaced and
//! permission-restricted. A corrupt or incompatible file is ignored by the
//! caller; it never prevents the buffer from opening.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use vanta_text::{ByteCol, LineCol, LineNumber, UndoEdit, UndoKind, UndoNode, UndoSnapshot};

const MAGIC: &[u8; 11] = b"VANTA-UNDO\0";
const VERSION: u32 = 1;
const MAX_PERSISTED_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: usize = 100_000;

/// Returns the undo-file path for `target`, or `None` when the platform has no
/// state directory.
pub(crate) fn undo_path(target: &Path) -> Option<PathBuf> {
    let directory = super::swap::state_root()?.join("undo");
    let identity = super::swap::canonical_identity(target);
    let hash = hash_bytes(identity.as_os_str().as_encoded_bytes());
    Some(directory.join(format!("{hash:016x}.undo")))
}

/// Loads a snapshot when the file is valid and belongs to `text`.
pub(crate) fn load(path: &Path, text: &str) -> Option<UndoSnapshot> {
    let bytes = fs::read(path).ok()?;
    decode(&bytes, text)
}

/// Writes a snapshot atomically, creating the undo directory as needed.
pub(crate) fn save(path: &Path, text: &str, snapshot: &UndoSnapshot) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "undo path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    restrict_directory(parent)?;
    let bytes = encode(text, snapshot)?;
    if bytes.len() > MAX_PERSISTED_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "undo history exceeds the persistence limit",
        ));
    }
    vanta_fs::save_atomic(path, &bytes).map_err(|source| {
        io::Error::other(format!(
            "cannot write undo file {}: {source}",
            path.display()
        ))
    })?;
    restrict_file(path)?;
    Ok(())
}

fn encode(text: &str, snapshot: &UndoSnapshot) -> io::Result<Vec<u8>> {
    if snapshot.nodes.len() > MAX_NODES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "undo tree has too many states",
        ));
    }
    let mut payload = Vec::new();
    push_u32(&mut payload, VERSION);
    push_u64(&mut payload, hash_bytes(text.as_bytes()));
    push_u64(
        &mut payload,
        u64::try_from(snapshot.current).unwrap_or(u64::MAX),
    );
    push_i64(
        &mut payload,
        snapshot.saved.map_or(-1, |saved| saved as i64),
    );
    push_u32(
        &mut payload,
        u32::try_from(snapshot.nodes.len()).unwrap_or(u32::MAX),
    );
    for node in &snapshot.nodes {
        push_i64(&mut payload, node.parent.map_or(-1, |parent| parent as i64));
        push_u32(
            &mut payload,
            u32::try_from(node.edits.len()).unwrap_or(u32::MAX),
        );
        push_position(&mut payload, node.cursor_before);
        push_position(&mut payload, node.cursor_after);
        push_u64(&mut payload, node.sequence);
        push_u64(&mut payload, node.timestamp);
        payload.push(kind_byte(node.kind));
        push_i64(
            &mut payload,
            node.last_child.map_or(-1, |child| child as i64),
        );
        for edit in &node.edits {
            push_u64(&mut payload, u64::try_from(edit.start).unwrap_or(u64::MAX));
            push_string(&mut payload, &edit.removed)?;
            push_string(&mut payload, &edit.inserted)?;
        }
    }

    let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + payload.len() + 8);
    bytes.extend_from_slice(MAGIC);
    push_u64(&mut bytes, u64::try_from(payload.len()).unwrap_or(u64::MAX));
    bytes.extend_from_slice(&payload);
    push_u64(&mut bytes, hash_bytes(&payload));
    Ok(bytes)
}

fn decode(bytes: &[u8], text: &str) -> Option<UndoSnapshot> {
    if bytes.len() < MAGIC.len() + 16 || &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    let mut reader = Reader {
        bytes,
        offset: MAGIC.len(),
    };
    let payload_len = usize::try_from(reader.u64()?).ok()?;
    if payload_len > MAX_PERSISTED_BYTES || reader.remaining() != payload_len + 8 {
        return None;
    }
    let payload_start = reader.offset;
    let payload = &bytes[payload_start..payload_start + payload_len];
    reader.offset += payload_len;
    let checksum = reader.u64()?;
    if checksum != hash_bytes(payload) {
        return None;
    }
    let mut reader = Reader {
        bytes: payload,
        offset: 0,
    };
    if reader.u32()? != VERSION || reader.u64()? != hash_bytes(text.as_bytes()) {
        return None;
    }
    let current = usize::try_from(reader.u64()?).ok()?;
    let saved_raw = reader.i64()?;
    let saved = if saved_raw < 0 {
        None
    } else {
        Some(usize::try_from(saved_raw).ok()?)
    };
    let count = usize::try_from(reader.u32()?).ok()?;
    if count == 0 || count > MAX_NODES {
        return None;
    }
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        let parent_raw = reader.i64()?;
        let parent = if parent_raw < 0 {
            None
        } else {
            Some(usize::try_from(parent_raw).ok()?)
        };
        let edit_count = usize::try_from(reader.u32()?).ok()?;
        if edit_count > 1_000_000 {
            return None;
        }
        let cursor_before = LineCol {
            line: LineNumber::new(usize::try_from(reader.u64()?).ok()?),
            col: ByteCol::new(usize::try_from(reader.u64()?).ok()?),
        };
        let cursor_after = LineCol {
            line: LineNumber::new(usize::try_from(reader.u64()?).ok()?),
            col: ByteCol::new(usize::try_from(reader.u64()?).ok()?),
        };
        let sequence = reader.u64()?;
        let timestamp = reader.u64()?;
        let kind = kind_from_byte(reader.byte()?)?;
        let last_child_raw = reader.i64()?;
        let last_child = if last_child_raw < 0 {
            None
        } else {
            Some(usize::try_from(last_child_raw).ok()?)
        };
        let mut edits = Vec::with_capacity(edit_count);
        for _ in 0..edit_count {
            edits.push(UndoEdit {
                start: usize::try_from(reader.u64()?).ok()?,
                removed: reader.string()?,
                inserted: reader.string()?,
            });
        }
        nodes.push(UndoNode {
            parent,
            edits,
            cursor_before,
            cursor_after,
            sequence,
            timestamp,
            kind,
            last_child,
        });
    }
    if reader.remaining() != 0 {
        return None;
    }
    Some(UndoSnapshot {
        version: VERSION,
        nodes,
        current,
        saved,
    })
}

fn push_position(output: &mut Vec<u8>, position: LineCol) {
    push_u64(output, position.line.get() as u64);
    push_u64(output, position.col.get() as u64);
}

fn kind_byte(kind: UndoKind) -> u8 {
    match kind {
        UndoKind::Insert => 0,
        UndoKind::Delete => 1,
        UndoKind::Change => 2,
        UndoKind::Indent => 3,
        UndoKind::Paste => 4,
        UndoKind::Block => 5,
        UndoKind::Filter => 6,
    }
}

fn kind_from_byte(byte: u8) -> Option<UndoKind> {
    Some(match byte {
        0 => UndoKind::Insert,
        1 => UndoKind::Delete,
        2 => UndoKind::Change,
        3 => UndoKind::Indent,
        4 => UndoKind::Paste,
        5 => UndoKind::Block,
        6 => UndoKind::Filter,
        _ => return None,
    })
}

fn push_string(output: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length = u32::try_from(value.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "undo string is too large"))?;
    push_u32(output, length);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.offset)?;
        self.offset += 1;
        Some(byte)
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(length)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let length = usize::try_from(self.u32()?).ok()?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).ok()
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanta_text::Buffer;

    fn snapshot(text: &str) -> UndoSnapshot {
        let mut buffer = Buffer::from_text(text);
        let edit = buffer
            .apply(vanta_text::BytePos::ZERO..vanta_text::BytePos::ZERO, "x")
            .expect("edit");
        buffer.record_change_kind(
            edit,
            LineCol::default(),
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(1),
            },
            UndoKind::Insert,
        );
        buffer.undo_snapshot()
    }

    #[test]
    fn round_trip_preserves_nodes_and_checksum() {
        let snapshot = snapshot("body");
        let bytes = encode("xbody", &snapshot).expect("encode");
        assert_eq!(decode(&bytes, "xbody"), Some(snapshot.clone()));
        assert!(decode(&bytes, "different").is_none());
    }

    #[test]
    fn truncation_and_corruption_are_rejected() {
        let snapshot = snapshot("body");
        let mut bytes = encode("xbody", &snapshot).expect("encode");
        bytes.pop();
        assert!(decode(&bytes, "xbody").is_none());
        let mut bytes = encode("xbody", &snapshot).expect("encode");
        let last = bytes.len() - 9;
        bytes[last] ^= 0xff;
        assert!(decode(&bytes, "xbody").is_none());
    }
}
