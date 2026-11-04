//! Motions and text objects over the buffer.
//!
//! Every motion reports a canonical target and an extent. Operators use the
//! extent to decide whether the target character is included or whether whole
//! lines are addressed. Text objects resolve directly to [`TextRange`] values,
//! so operators and Visual mode do not grow separate range implementations.

use unicode_segmentation::UnicodeSegmentation;
use vanta_text::{
    ByteCol, BytePos, DisplayCol, LineCol, LineNumber, TextError, byte_of_display_col,
    next_grapheme_boundary, prev_grapheme_boundary,
};

use super::{Document, TextRange};

/// A cursor motion used by Normal and Operator-pending modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Right,
    Down,
    Up,
    WordForward {
        big: bool,
    },
    WordBack {
        big: bool,
    },
    WordEnd {
        big: bool,
    },
    WordEndBack {
        big: bool,
    },
    LineStart,
    FirstNonBlank,
    LineEnd,
    ScreenColumn,
    Find {
        character: char,
        forward: bool,
        until: bool,
    },
    RepeatFind {
        reverse: bool,
    },
    MatchPair,
    ParagraphForward,
    ParagraphBack,
    GotoLine,
    GotoFileStart,
    SearchNext,
    SearchPrevious,
    WordUnderCursor {
        forward: bool,
    },
}

/// How an operator consumes a motion target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionExtent {
    /// The character at the target is not included.
    Exclusive,
    /// The character at the target is included.
    Inclusive,
    /// Whole lines from the start through the target line are addressed.
    Linewise,
}

/// A resolved motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MotionTarget {
    pub(crate) position: LineCol,
    pub(crate) extent: MotionExtent,
    /// `cw` changes to the end of the current word rather than the next word.
    pub(crate) change_word_special: bool,
}

/// A text object selected after an operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextObject {
    Word { big: bool },
    Sentence,
    Paragraph,
    Pair(char),
    Quote(char),
    Tag,
}

impl TextObject {
    pub(crate) fn from_kind(character: char) -> Option<Self> {
        Some(match character {
            'w' => Self::Word { big: false },
            'W' => Self::Word { big: true },
            's' => Self::Sentence,
            'p' => Self::Paragraph,
            '(' | ')' | 'b' => Self::Pair('('),
            '[' | ']' => Self::Pair('['),
            '{' | '}' | 'B' => Self::Pair('{'),
            '<' | '>' => Self::Pair('<'),
            '"' | '\'' | '`' => Self::Quote(character),
            't' => Self::Tag,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punctuation,
}

fn class_of(grapheme: &str, big: bool) -> Class {
    if grapheme.chars().all(char::is_whitespace) {
        return Class::Space;
    }
    if big {
        return Class::Word;
    }
    let mut characters = grapheme.chars();
    let Some(first) = characters.next() else {
        return Class::Space;
    };
    if first.is_alphanumeric() || first == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

fn clusters(text: &str, big: bool) -> Vec<(usize, usize, Class)> {
    text.grapheme_indices(true)
        .map(|(start, grapheme)| (start, start + grapheme.len(), class_of(grapheme, big)))
        .collect()
}

impl Document {
    /// Resolves an operator target into the same characterwise or linewise
    /// range used by Visual mode and the range verbs.
    pub(crate) fn operator_range(
        &self,
        start: LineCol,
        target: MotionTarget,
    ) -> Result<Option<TextRange>, TextError> {
        let start_byte = self.buffer.line_col_to_byte(start)?;
        let target_byte = self.buffer.line_col_to_byte(target.position)?;
        let range = match target.extent {
            MotionExtent::Linewise => {
                let (first, last) = if start.line <= target.position.line {
                    (start.line, target.position.line)
                } else {
                    (target.position.line, start.line)
                };
                TextRange::Linewise(first..LineNumber::new(last.get() + 1))
            }
            MotionExtent::Exclusive => {
                if start_byte <= target_byte {
                    TextRange::Charwise(start_byte..target_byte)
                } else {
                    TextRange::Charwise(target_byte..start_byte)
                }
            }
            MotionExtent::Inclusive => {
                let line = self.effective_line(target.position.line);
                let text = self.buffer.line_text(line)?;
                let col = target.position.col.get().min(text.len());
                let end = if col < text.len() {
                    next_grapheme_boundary(&text, col).unwrap_or(text.len())
                } else {
                    col
                };
                let end_byte = BytePos::new(self.line_start(line)?.get() + end);
                if start_byte <= end_byte {
                    TextRange::Charwise(start_byte..end_byte)
                } else {
                    TextRange::Charwise(target_byte..start_byte)
                }
            }
        };
        if matches!(&range, TextRange::Charwise(bytes) if bytes.start == bytes.end) {
            return Ok(None);
        }
        if matches!(&range, TextRange::Linewise(lines) if lines.start >= lines.end) {
            return Ok(None);
        }
        Ok(Some(range))
    }

    /// Resolves `motion` `count` times without moving the cursor.
    ///
    /// Returns `None` for motions that need editor-owned state such as the
    /// previous search or the viewport.
    pub(crate) fn motion_target(
        &self,
        motion: Motion,
        count: usize,
    ) -> Result<Option<MotionTarget>, TextError> {
        let count = count.max(1);
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        let col = self.cursor.col.get().min(text.len());
        let mut target = match motion {
            Motion::Left => {
                let mut offset = col;
                for _ in 0..count {
                    offset = prev_grapheme_boundary(&text, offset).unwrap_or(0);
                }
                MotionTarget {
                    position: LineCol {
                        line,
                        col: ByteCol::new(offset),
                    },
                    extent: MotionExtent::Exclusive,
                    change_word_special: false,
                }
            }
            Motion::Right => {
                let mut offset = col;
                for _ in 0..count {
                    if offset >= text.len() {
                        break;
                    }
                    offset = next_grapheme_boundary(&text, offset).unwrap_or(text.len());
                }
                MotionTarget {
                    position: LineCol {
                        line,
                        col: ByteCol::new(
                            offset.min(prev_grapheme_boundary(&text, text.len()).unwrap_or(0)),
                        ),
                    },
                    extent: MotionExtent::Exclusive,
                    change_word_special: false,
                }
            }
            Motion::Down | Motion::Up => {
                let delta = if motion == Motion::Down {
                    count as isize
                } else {
                    -(count as isize)
                };
                let last = self.line_count().saturating_sub(1);
                let current = self.cursor.line.get().min(last) as isize;
                let target_line = (current + delta).clamp(0, last as isize) as usize;
                let preferred = self.cursor_display_column();
                let target_text = self.buffer.line_text(LineNumber::new(target_line))?;
                let target_col = crate::view::normal_column_at_display(&target_text, preferred);
                MotionTarget {
                    position: LineCol {
                        line: LineNumber::new(target_line),
                        col: ByteCol::new(target_col),
                    },
                    extent: MotionExtent::Linewise,
                    change_word_special: false,
                }
            }
            Motion::LineStart => MotionTarget {
                position: LineCol {
                    line,
                    col: ByteCol::ZERO,
                },
                extent: MotionExtent::Exclusive,
                change_word_special: false,
            },
            Motion::FirstNonBlank => MotionTarget {
                position: LineCol {
                    line,
                    col: ByteCol::new(first_non_blank(&text)),
                },
                extent: MotionExtent::Exclusive,
                change_word_special: false,
            },
            Motion::LineEnd => {
                let offset = match motion {
                    _ if text.is_empty() => 0,
                    _ => prev_grapheme_boundary(&text, text.len()).unwrap_or(0),
                };
                MotionTarget {
                    position: LineCol {
                        line,
                        col: ByteCol::new(offset),
                    },
                    extent: MotionExtent::Inclusive,
                    change_word_special: false,
                }
            }
            Motion::ScreenColumn => {
                let byte = byte_of_display_col(
                    &text,
                    DisplayCol::new(count.saturating_sub(1)),
                    crate::view::TABSTOP,
                );
                MotionTarget {
                    position: LineCol {
                        line,
                        col: ByteCol::new(byte),
                    },
                    extent: MotionExtent::Exclusive,
                    change_word_special: false,
                }
            }
            Motion::Find {
                character,
                forward,
                until,
            } => {
                let mut found = None;
                let mut seen = 0;
                if forward {
                    for (start, grapheme) in text.grapheme_indices(true) {
                        if start < col || grapheme != character.to_string() {
                            continue;
                        }
                        seen += 1;
                        if seen == count {
                            found = Some(start);
                            break;
                        }
                    }
                } else {
                    for (start, grapheme) in text.grapheme_indices(true).rev() {
                        if start >= col || grapheme != character.to_string() {
                            continue;
                        }
                        seen += 1;
                        if seen == count {
                            found = Some(start);
                            break;
                        }
                    }
                }
                let Some(found) = found else {
                    return Ok(None);
                };
                let offset = if until {
                    if forward {
                        prev_grapheme_boundary(&text, found).unwrap_or(found)
                    } else {
                        next_grapheme_boundary(&text, found).unwrap_or(text.len())
                    }
                } else {
                    found
                };
                MotionTarget {
                    position: LineCol {
                        line,
                        col: ByteCol::new(offset),
                    },
                    extent: MotionExtent::Inclusive,
                    change_word_special: false,
                }
            }
            Motion::WordForward { big } => {
                let mut cursor = self.cursor;
                for _ in 0..count {
                    cursor = self.word_forward(cursor, big)?;
                }
                MotionTarget {
                    position: cursor,
                    extent: MotionExtent::Exclusive,
                    change_word_special: false,
                }
            }
            Motion::WordBack { big } => {
                let mut cursor = self.cursor;
                for _ in 0..count {
                    cursor = self.word_back(cursor, big)?;
                }
                MotionTarget {
                    position: cursor,
                    extent: MotionExtent::Exclusive,
                    change_word_special: false,
                }
            }
            Motion::WordEnd { big } => {
                let mut cursor = self.cursor;
                for _ in 0..count {
                    cursor = self.word_end(cursor, big)?;
                }
                MotionTarget {
                    position: cursor,
                    extent: MotionExtent::Inclusive,
                    change_word_special: true,
                }
            }
            Motion::WordEndBack { big } => {
                let mut cursor = self.cursor;
                for _ in 0..count {
                    cursor = self.word_end_back(cursor, big)?;
                }
                MotionTarget {
                    position: cursor,
                    extent: MotionExtent::Inclusive,
                    change_word_special: false,
                }
            }
            Motion::GotoLine => {
                let last = self.line_count().saturating_sub(1);
                let target_line = if count > 1 {
                    count.saturating_sub(1).min(last)
                } else {
                    last
                };
                MotionTarget {
                    position: LineCol {
                        line: LineNumber::new(target_line),
                        col: ByteCol::ZERO,
                    },
                    extent: MotionExtent::Linewise,
                    change_word_special: false,
                }
            }
            Motion::GotoFileStart => {
                let target_line = if count > 1 {
                    count
                        .saturating_sub(1)
                        .min(self.line_count().saturating_sub(1))
                } else {
                    0
                };
                MotionTarget {
                    position: LineCol {
                        line: LineNumber::new(target_line),
                        col: ByteCol::ZERO,
                    },
                    extent: MotionExtent::Linewise,
                    change_word_special: false,
                }
            }
            Motion::ParagraphForward => {
                let mut target = self.cursor.line;
                for _ in 0..count {
                    target = self.paragraph_forward(target)?;
                }
                MotionTarget {
                    position: LineCol {
                        line: target,
                        col: ByteCol::ZERO,
                    },
                    extent: MotionExtent::Linewise,
                    change_word_special: false,
                }
            }
            Motion::ParagraphBack => {
                let mut target = self.cursor.line;
                for _ in 0..count {
                    target = self.paragraph_back(target)?;
                }
                MotionTarget {
                    position: LineCol {
                        line: target,
                        col: ByteCol::ZERO,
                    },
                    extent: MotionExtent::Linewise,
                    change_word_special: false,
                }
            }
            Motion::MatchPair => {
                let Some(offset) = self.match_pair_offset()? else {
                    return Ok(None);
                };
                MotionTarget {
                    position: self.buffer.byte_to_line_col(offset)?,
                    extent: MotionExtent::Inclusive,
                    change_word_special: false,
                }
            }
            Motion::RepeatFind { .. }
            | Motion::SearchNext
            | Motion::SearchPrevious
            | Motion::WordUnderCursor { .. } => return Ok(None),
        };
        if matches!(motion, Motion::LineEnd) {
            target.extent = MotionExtent::Inclusive;
        }
        Ok(Some(target))
    }

    /// Moves to a resolved motion target.
    pub(crate) fn move_to_motion(&mut self, target: MotionTarget) -> Result<(), TextError> {
        self.move_to(self.buffer.line_col_to_byte(target.position)?)?;
        Ok(())
    }

    fn word_forward(&self, cursor: LineCol, big: bool) -> Result<LineCol, TextError> {
        let line = self.effective_line(cursor.line);
        let text = self.buffer.line_text(line)?;
        let items = clusters(&text, big);
        let col = cursor.col.get().min(text.len());
        let mut index = items.partition_point(|(_, end, _)| *end <= col);
        if index >= items.len() {
            return self.next_line_start(line);
        }
        let class = items[index].2;
        while index < items.len() && items[index].2 == class {
            index += 1;
        }
        while index < items.len() && items[index].2 == Class::Space {
            index += 1;
        }
        if index >= items.len() {
            return self.next_line_start(line);
        }
        Ok(LineCol {
            line,
            col: ByteCol::new(items[index].0),
        })
    }

    fn word_back(&self, cursor: LineCol, big: bool) -> Result<LineCol, TextError> {
        let line = self.effective_line(cursor.line);
        let text = self.buffer.line_text(line)?;
        let items = clusters(&text, big);
        let col = cursor.col.get().min(text.len());
        let mut index = items.partition_point(|(_, end, _)| *end <= col);
        while index > 0 && items[index - 1].2 == Class::Space {
            index -= 1;
        }
        if index > 0 {
            let class = items[index - 1].2;
            while index > 0 && items[index - 1].2 == class {
                index -= 1;
            }
            return Ok(LineCol {
                line,
                col: ByteCol::new(items[index].0),
            });
        }
        if line.get() == 0 {
            return Ok(LineCol {
                line,
                col: ByteCol::ZERO,
            });
        }
        let previous = LineNumber::new(line.get() - 1);
        let previous_text = self.buffer.line_text(previous)?;
        let items = clusters(&previous_text, big);
        let mut index = items.len();
        while index > 0 && items[index - 1].2 == Class::Space {
            index -= 1;
        }
        if index > 0 {
            let class = items[index - 1].2;
            while index > 0 && items[index - 1].2 == class {
                index -= 1;
            }
            return Ok(LineCol {
                line: previous,
                col: ByteCol::new(items[index].0),
            });
        }
        Ok(LineCol {
            line: previous,
            col: ByteCol::ZERO,
        })
    }

    fn word_end(&self, cursor: LineCol, big: bool) -> Result<LineCol, TextError> {
        let line = self.effective_line(cursor.line);
        let text = self.buffer.line_text(line)?;
        let items = clusters(&text, big);
        let col = cursor.col.get().min(text.len());
        let mut index = items.partition_point(|(_, end, _)| *end <= col);
        while index < items.len() && items[index].2 == Class::Space {
            index += 1;
        }
        if index >= items.len() {
            if line.get() + 1 >= self.line_count() {
                return Ok(cursor);
            }
            return self.word_end(
                LineCol {
                    line: LineNumber::new(line.get() + 1),
                    col: ByteCol::ZERO,
                },
                big,
            );
        }
        let class = items[index].2;
        while index + 1 < items.len() && items[index + 1].2 == class {
            index += 1;
        }
        Ok(LineCol {
            line,
            col: ByteCol::new(items[index].0),
        })
    }

    fn word_end_back(&self, cursor: LineCol, big: bool) -> Result<LineCol, TextError> {
        let line = self.effective_line(cursor.line);
        let text = self.buffer.line_text(line)?;
        let items = clusters(&text, big);
        let col = cursor.col.get().min(text.len());
        let mut index = items.partition_point(|(_, end, _)| *end <= col);
        while index > 0 && items[index - 1].2 == Class::Space {
            index -= 1;
        }
        if index > 0 && items[index - 1].2 != Class::Space {
            return Ok(LineCol {
                line,
                col: ByteCol::new(items[index - 1].0),
            });
        }
        if line.get() == 0 {
            return Ok(LineCol::default());
        }
        let previous = LineNumber::new(line.get() - 1);
        let previous_text = self.buffer.line_text(previous)?;
        let previous_items = clusters(&previous_text, big);
        let index = previous_items.len().saturating_sub(1);
        Ok(LineCol {
            line: previous,
            col: ByteCol::new(previous_items.get(index).map_or(0, |item| item.0)),
        })
    }

    fn next_line_start(&self, line: LineNumber) -> Result<LineCol, TextError> {
        let mut next = line.get() + 1;
        while next < self.line_count() {
            let target = LineNumber::new(next);
            let text = self.buffer.line_text(target)?;
            if !text.is_empty() {
                return Ok(LineCol {
                    line: target,
                    col: ByteCol::new(first_non_blank(&text)),
                });
            }
            next += 1;
        }
        let last = self.line_count().saturating_sub(1);
        Ok(LineCol {
            line: LineNumber::new(last),
            col: ByteCol::ZERO,
        })
    }

    fn paragraph_forward(&self, line: LineNumber) -> Result<LineNumber, TextError> {
        let mut next = line.get() + 1;
        while next < self.line_count() {
            let target = LineNumber::new(next);
            if self.buffer.line_text(target)?.is_empty() {
                return Ok(target);
            }
            next += 1;
        }
        Ok(LineNumber::new(self.line_count().saturating_sub(1)))
    }

    fn paragraph_back(&self, line: LineNumber) -> Result<LineNumber, TextError> {
        let mut previous = line.get();
        while previous > 0 {
            previous -= 1;
            let target = LineNumber::new(previous);
            if self.buffer.line_text(target)?.is_empty() {
                return Ok(target);
            }
        }
        Ok(LineNumber::ZERO)
    }

    fn match_pair_offset(&self) -> Result<Option<BytePos>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        let line_start = self.line_start(line)?;
        let col = self.cursor.col.get().min(text.len());
        let bytes = text.as_bytes();
        let Some(offset) = (col..bytes.len())
            .find(|offset| matches!(bytes[*offset], b'(' | b')' | b'[' | b']' | b'{' | b'}'))
        else {
            return Ok(None);
        };
        let open = bytes[offset];
        let close = match open {
            b'(' => Some((b'(', b')')),
            b'[' => Some((b'[', b']')),
            b'{' => Some((b'{', b'}')),
            b')' | b']' | b'}' => None,
            _ => None,
        };
        if let Some((open, close)) = close {
            let mut depth = 0usize;
            for (index, byte) in bytes.iter().enumerate().skip(offset) {
                if *byte == open {
                    depth += 1;
                } else if *byte == close {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(Some(BytePos::new(line_start.get() + index)));
                    }
                }
            }
        } else {
            let open = match open {
                b')' => b'(',
                b']' => b'[',
                b'}' => b'{',
                _ => return Ok(None),
            };
            let close = bytes[offset];
            let mut depth = 0usize;
            for index in (0..=offset).rev() {
                if bytes[index] == close {
                    depth += 1;
                } else if bytes[index] == open {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(Some(BytePos::new(line_start.get() + index)));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Resolves a text object at the cursor.
    pub(crate) fn text_object(
        &self,
        object: TextObject,
        around: bool,
        count: usize,
    ) -> Result<Option<TextRange>, TextError> {
        let range = match object {
            TextObject::Word { big } => self.word_object(big, around),
            TextObject::Paragraph => self.paragraph_object(around),
            TextObject::Quote(quote) => self.quote_object(quote, around),
            TextObject::Pair(open) => self.pair_object(open, around),
            TextObject::Sentence => self.sentence_object(around),
            TextObject::Tag => self.tag_object(around),
        }?;
        let Some(range) = range else {
            return Ok(None);
        };
        if count <= 1 {
            return Ok(Some(range));
        }
        let _ = count;
        Ok(Some(range))
    }

    fn word_object(&self, big: bool, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        if text.is_empty() {
            return Ok(None);
        }
        let items = clusters(&text, big);
        let col = self.cursor.col.get().min(text.len());
        let index = items
            .partition_point(|(_, end, _)| *end <= col)
            .min(items.len().saturating_sub(1));
        let class = items[index].2;
        let mut start = index;
        while start > 0 && items[start - 1].2 == class {
            start -= 1;
        }
        let mut end = index + 1;
        while end < items.len() && items[end].2 == class {
            end += 1;
        }
        if around {
            while end < items.len() && items[end].2 == Class::Space {
                end += 1;
            }
            if end == index + 1 {
                while start > 0 && items[start - 1].2 == Class::Space {
                    start -= 1;
                }
            }
        }
        let start = items[start].0;
        let end = items.get(end).map_or(text.len(), |item| item.0);
        let line_start = self.line_start(line)?;
        Ok(Some(TextRange::Charwise(
            BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + end),
        )))
    }

    fn paragraph_object(&self, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let mut first = line;
        while first.get() > 0 {
            let previous = LineNumber::new(first.get() - 1);
            if self.buffer.line_text(previous)?.is_empty() {
                break;
            }
            first = previous;
        }
        let mut last = line;
        while last.get() + 1 < self.line_count() {
            let next = LineNumber::new(last.get() + 1);
            if self.buffer.line_text(next)?.is_empty() {
                break;
            }
            last = next;
        }
        if around {
            while last.get() + 1 < self.line_count() {
                let next = LineNumber::new(last.get() + 1);
                if !self.buffer.line_text(next)?.is_empty() {
                    break;
                }
                last = next;
            }
        }
        Ok(Some(TextRange::Linewise(
            first..LineNumber::new(last.get() + 1),
        )))
    }

    fn pair_object(&self, open: char, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '<' => '>',
            _ => return Ok(None),
        };
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let col = self.cursor.col.get().min(text.len());
        let cursor_index = chars
            .partition_point(|(offset, _)| *offset < col)
            .min(chars.len());
        let mut stack = Vec::new();
        let mut best = None;
        for (index, (_, character)) in chars.iter().enumerate() {
            if *character == open {
                stack.push(index);
            } else if *character == close
                && let Some(start) = stack.pop()
                && start < cursor_index
                && index >= cursor_index
            {
                best = Some((start, index));
                break;
            }
        }
        if best.is_none() {
            for (index, (_, character)) in chars.iter().enumerate() {
                if *character == open {
                    let mut depth = 0usize;
                    for (end, (_, candidate)) in chars.iter().enumerate().skip(index) {
                        if *candidate == open {
                            depth += 1;
                        } else if *candidate == close {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                best = Some((index, end));
                                break;
                            }
                        }
                    }
                    if best.is_some() {
                        break;
                    }
                }
            }
        }
        let Some((start, end)) = best else {
            return Ok(None);
        };
        let (start, end) = if around {
            (chars[start].0, chars[end].0 + close.len_utf8())
        } else if end > start + 1 {
            (chars[start].0 + open.len_utf8(), chars[end].0)
        } else {
            (chars[start].0, chars[end].0)
        };
        let line_start = self.line_start(line)?;
        Ok(Some(TextRange::Charwise(
            BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + end),
        )))
    }

    fn quote_object(&self, quote: char, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        let offsets: Vec<usize> = text
            .char_indices()
            .filter(|(_, character)| *character == quote)
            .map(|(offset, _)| offset)
            .collect();
        if offsets.len() < 2 {
            return Ok(None);
        }
        let cursor = self.cursor.col.get().min(text.len());
        let mut pair = None;
        for pair_offsets in offsets.chunks_exact(2) {
            if cursor <= pair_offsets[1] {
                pair = Some((pair_offsets[0], pair_offsets[1]));
                break;
            }
        }
        let Some((open, close)) = pair else {
            return Ok(None);
        };
        let (start, end) = if around {
            (open, close + quote.len_utf8())
        } else {
            (open + quote.len_utf8(), close)
        };
        let line_start = self.line_start(line)?;
        Ok(Some(TextRange::Charwise(
            BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + end),
        )))
    }

    fn sentence_object(&self, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        if text.is_empty() {
            return Ok(None);
        }
        let mut start = 0;
        let mut end = text.len();
        let bytes = text.as_bytes();
        for (offset, character) in text.char_indices().rev() {
            if offset < self.cursor.col.get()
                && matches!(character, '.' | '!' | '?')
                && bytes.get(offset + 1) == Some(&b' ')
            {
                start = offset + 2;
                break;
            }
        }
        for (offset, character) in text.char_indices() {
            if offset >= self.cursor.col.get() && matches!(character, '.' | '!' | '?') {
                end = offset + 1;
                break;
            }
        }
        if around {
            while end < text.len() && text.as_bytes()[end] == b' ' {
                end += 1;
            }
        }
        let line_start = self.line_start(line)?;
        Ok(Some(TextRange::Charwise(
            BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + end),
        )))
    }

    fn tag_object(&self, around: bool) -> Result<Option<TextRange>, TextError> {
        let line = self.effective_line(self.cursor.line);
        let text = self.buffer.line_text(line)?;
        let cursor = self.cursor.col.get().min(text.len());
        let Some(open) = text[..cursor].rfind('<') else {
            return Ok(None);
        };
        let Some(close) = text[open..].find('>') else {
            return Ok(None);
        };
        let close = open + close;
        let name = text[open + 1..close].trim_start_matches('/');
        let closing = format!("</{name}>");
        let Some(relative) = text[close + 1..].find(&closing) else {
            return Ok(None);
        };
        let content_end = close + 1 + relative;
        let (start, end) = if around {
            (open, content_end + closing.len())
        } else {
            (close + 1, content_end)
        };
        let line_start = self.line_start(line)?;
        Ok(Some(TextRange::Charwise(
            BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + end),
        )))
    }
}

fn first_non_blank(text: &str) -> usize {
    text.char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(0, |(offset, _)| offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn document(text: &str) -> Document {
        Document::from_parts(None, text)
    }

    #[test]
    fn word_motion_crosses_lines_and_counts() {
        let doc = document("one two\nthree\n");
        let target = doc
            .motion_target(Motion::WordForward { big: false }, 2)
            .expect("motion")
            .expect("supported");
        assert_eq!(target.position.line.get(), 1);
        assert_eq!(target.position.col.get(), 0);
        assert_eq!(target.extent, MotionExtent::Exclusive);
    }

    #[test]
    fn dollar_is_inclusive_and_forward_word_is_exclusive() {
        let doc = document("alpha beta\n");
        let dollar = doc
            .motion_target(Motion::LineEnd, 1)
            .expect("motion")
            .expect("supported");
        assert_eq!(dollar.extent, MotionExtent::Inclusive);
        let word = doc
            .motion_target(Motion::WordForward { big: false }, 1)
            .expect("motion")
            .expect("supported");
        assert_eq!(word.extent, MotionExtent::Exclusive);
    }

    #[test]
    fn bracket_motion_and_objects_use_the_same_pair() {
        let doc = document("call(alpha, beta)\n");
        let range = doc
            .text_object(TextObject::Pair('('), false, 1)
            .expect("object")
            .expect("enclosed");
        let TextRange::Charwise(range) = range else {
            panic!("pair object is characterwise");
        };
        assert_eq!(
            doc.buffer().slice_text(range).expect("slice"),
            "alpha, beta"
        );
    }

    #[test]
    fn paragraph_objects_cover_contiguous_nonblank_lines() {
        let doc = document("one\ntwo\n\nthree\n");
        let range = doc
            .text_object(TextObject::Paragraph, false, 1)
            .expect("object")
            .expect("paragraph");
        let TextRange::Linewise(lines) = range else {
            panic!("paragraph object is linewise");
        };
        assert_eq!(lines, LineNumber::new(0)..LineNumber::new(2));
    }

    #[test]
    fn first_non_blank_skips_indent() {
        let doc = document("    body\n");
        let target = doc
            .motion_target(Motion::FirstNonBlank, 1)
            .expect("motion")
            .expect("supported");
        assert_eq!(target.position.col.get(), 4);
    }
}
