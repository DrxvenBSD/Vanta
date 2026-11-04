//! The Normal-mode command grammar shared by live keys, macros and `:normal`.
//!
//! The parser is deliberately independent of editor state. It turns a key
//! stream into a complete command, a valid prefix that needs another key, or an
//! invalid sequence. Execution and buffer mutation live in `Editor`; this
//! module only enforces the grammar and the count/register composition rules.

use crate::document::{Motion, TextObject};
use crate::terminal::InputEvent;

/// An operator that consumes a motion, text object or repeated line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
    ShiftRight,
    ShiftLeft,
    Reindent,
    ToggleCase,
    Lowercase,
    Uppercase,
    Rot13,
    Format,
}

/// Where an operator gets its range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Motion {
        count: usize,
        motion: Motion,
    },
    TextObject {
        count: usize,
        around: bool,
        object: TextObject,
    },
    Line,
}

/// Insert entry command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InsertEntry {
    Before,
    After,
    LineStart,
    LineEnd,
    OpenBelow,
    OpenAbove,
    FirstNonBlank,
    LastInsert,
}

/// Visual selection shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionMode {
    Charwise,
    Linewise,
    Blockwise,
}

/// Window commands reached through `<C-w>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowAction {
    SplitBelow,
    SplitRight,
    Next,
    Previous,
    Left,
    Right,
    Up,
    Down,
    Close,
    Only,
    Equalize,
    Taller,
    Shorter,
    Wider,
    Narrower,
    Rotate,
}

/// A Normal-mode action that does not consume a motion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Insert(InsertEntry),
    Replace,
    Visual(SelectionMode),
    Select(SelectionMode),
    CommandLine,
    SearchForward,
    SearchBackward,
    DeleteChar { before: bool, count: usize },
    DeleteToEnd { count: usize },
    ChangeToEnd { count: usize },
    ChangeLine { count: usize },
    YankLine { count: usize },
    Put { before: bool, count: usize },
    Join { spaces: bool, count: usize },
    ToggleCase { count: usize },
    Undo,
    Redo,
    UndoEarlier,
    UndoLater,
    RestoreLine,
    RepeatChange,
    ReselectVisual,
    Record(Option<char>),
    Replay(char),
    RepeatMacro,
    TabNext { count: usize, forward: bool },
    Window(WindowAction),
    Redraw,
    FileInfo,
    Quit { write: bool },
}

/// One complete Normal-mode command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Move {
        count: usize,
        motion: Motion,
    },
    Operator {
        count: usize,
        operator: Operator,
        register: Option<char>,
        target: Target,
    },
    Action {
        count: usize,
        register: Option<char>,
        action: Action,
    },
}

/// The result of feeding one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    Complete(Command),
    Invalid,
}

/// Incremental Normal-mode parser.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parser {
    keys: Vec<InputEvent>,
}

impl Parser {
    /// Feeds one key and returns the updated parse state.
    pub(crate) fn feed(&mut self, event: InputEvent) -> Outcome {
        self.keys.push(event);
        match parse(&self.keys) {
            Outcome::Pending => Outcome::Pending,
            complete => {
                self.keys.clear();
                complete
            }
        }
    }

    /// Discards a pending operator, count or prefix.
    pub(crate) fn reset(&mut self) {
        self.keys.clear();
    }

    /// Returns whether a sequence is pending.
    pub(crate) fn is_pending(&self) -> bool {
        !self.keys.is_empty()
    }
}

fn parse(keys: &[InputEvent]) -> Outcome {
    if keys.is_empty() {
        return Outcome::Pending;
    }
    let Some((mut count, mut index)) = parse_count(keys) else {
        return Outcome::Invalid;
    };
    let mut register = None;
    if matches!(keys.get(index), Some(InputEvent::Char('"'))) {
        index += 1;
        let Some(InputEvent::Char(name)) = keys.get(index).copied() else {
            return Outcome::Pending;
        };
        if name == '=' || name == ':' {
            return Outcome::Invalid;
        }
        register = Some(name);
        index += 1;
    }
    if register.is_some()
        && let Some((suffix, consumed)) = parse_count(&keys[index..])
        && consumed > 0
    {
        count = count.saturating_mul(suffix);
        index += consumed;
    }
    let Some(first) = keys.get(index).copied() else {
        return Outcome::Pending;
    };

    if let Some((operator, consumed)) = parse_operator(keys, index) {
        index += consumed;
        if index >= keys.len() {
            return Outcome::Pending;
        }
        let operator_count;
        if let Some((value, consumed)) = parse_count(&keys[index..]) {
            operator_count = value;
            index += consumed;
        } else if matches!(keys[index], InputEvent::Char('0')) {
            operator_count = 0;
        } else {
            operator_count = 1;
        }
        let Some(next) = keys.get(index).copied() else {
            return Outcome::Pending;
        };

        if repeats_operator(operator, next) {
            let total = count.saturating_mul(operator_count.max(1));
            return Outcome::Complete(Command::Operator {
                count: total,
                operator,
                register,
                target: Target::Line,
            });
        }

        let target = if matches!(next, InputEvent::Char('i' | 'a')) {
            let Some(InputEvent::Char(kind)) = keys.get(index + 1).copied() else {
                return Outcome::Pending;
            };
            let Some(object) = TextObject::from_kind(kind) else {
                return Outcome::Invalid;
            };
            Target::TextObject {
                count: count.saturating_mul(operator_count.max(1)),
                around: next == InputEvent::Char('a'),
                object,
            }
        } else {
            match parse_motion(keys, index) {
                MotionParse::Complete(motion, _consumed) => Target::Motion {
                    count: count.saturating_mul(operator_count.max(1)),
                    motion,
                },
                MotionParse::Pending => return Outcome::Pending,
                MotionParse::Invalid => return Outcome::Invalid,
            }
        };
        return Outcome::Complete(Command::Operator {
            count,
            operator,
            register,
            target,
        });
    }

    match first {
        InputEvent::Char('i') => complete(Action::Insert(InsertEntry::Before), count, register),
        InputEvent::Char('a') => complete(Action::Insert(InsertEntry::After), count, register),
        InputEvent::Char('I') => complete(Action::Insert(InsertEntry::LineStart), count, register),
        InputEvent::Char('A') => complete(Action::Insert(InsertEntry::LineEnd), count, register),
        InputEvent::Char('o') => complete(Action::Insert(InsertEntry::OpenBelow), count, register),
        InputEvent::Char('O') => complete(Action::Insert(InsertEntry::OpenAbove), count, register),
        InputEvent::Char('R') => complete(Action::Replace, count, register),
        InputEvent::Char('v') => complete(Action::Visual(SelectionMode::Charwise), count, register),
        InputEvent::Char('V') => complete(Action::Visual(SelectionMode::Linewise), count, register),
        InputEvent::Ctrl('v') | InputEvent::Ctrl('q') => {
            complete(Action::Visual(SelectionMode::Blockwise), count, register)
        }
        InputEvent::Char(':') => complete(Action::CommandLine, count, register),
        InputEvent::Char('/') => complete(Action::SearchForward, count, register),
        InputEvent::Char('?') => complete(Action::SearchBackward, count, register),
        InputEvent::Char('x') | InputEvent::Char('X') => complete(
            Action::DeleteChar {
                before: first == InputEvent::Char('X'),
                count,
            },
            count,
            register,
        ),
        InputEvent::Char('D') => complete(Action::DeleteToEnd { count }, count, register),
        InputEvent::Char('C') => complete(Action::ChangeToEnd { count }, count, register),
        InputEvent::Char('Y') => complete(Action::YankLine { count }, count, register),
        InputEvent::Char('p') => complete(
            Action::Put {
                before: false,
                count,
            },
            count,
            register,
        ),
        InputEvent::Char('P') => complete(
            Action::Put {
                before: true,
                count,
            },
            count,
            register,
        ),
        InputEvent::Char('J') => complete(
            Action::Join {
                spaces: true,
                count,
            },
            count,
            register,
        ),
        InputEvent::Char('~') => complete(Action::ToggleCase { count }, count, register),
        InputEvent::Char('u') => complete(Action::Undo, count, register),
        InputEvent::Ctrl('r') => complete(Action::Redo, count, register),
        InputEvent::Char('U') => complete(Action::RestoreLine, count, register),
        InputEvent::Char('.') => complete(Action::RepeatChange, count, register),
        InputEvent::Char('q') => {
            let Some(InputEvent::Char(name)) = keys.get(index + 1).copied() else {
                return Outcome::Pending;
            };
            complete(Action::Record(Some(name)), count, register)
        }
        InputEvent::Char('@') => {
            let Some(InputEvent::Char(name)) = keys.get(index + 1).copied() else {
                return Outcome::Pending;
            };
            complete(
                if name == '@' {
                    Action::RepeatMacro
                } else {
                    Action::Replay(name)
                },
                count,
                register,
            )
        }
        InputEvent::Char('Z') => match keys.get(index + 1).copied() {
            Some(InputEvent::Char('Z')) => complete(Action::Quit { write: true }, count, register),
            Some(InputEvent::Char('Q')) => complete(Action::Quit { write: false }, count, register),
            Some(_) => Outcome::Invalid,
            None => Outcome::Pending,
        },
        InputEvent::Char('s') => complete(Action::ChangeToEnd { count }, count, register),
        InputEvent::Char('S') => complete(Action::ChangeLine { count }, count, register),
        InputEvent::Ctrl('w') => match keys.get(index + 1).copied() {
            Some(InputEvent::Char(character)) => {
                let Some(action) = window_action(character) else {
                    return Outcome::Invalid;
                };
                complete(Action::Window(action), count, register)
            }
            Some(_) => Outcome::Invalid,
            None => Outcome::Pending,
        },
        InputEvent::Ctrl('l') => complete(Action::Redraw, count, register),
        InputEvent::Ctrl('g') => complete(Action::FileInfo, count, register),
        _ => {
            if let Some((mut action, consumed)) = parse_g_action(keys, index) {
                if index + consumed < keys.len() {
                    return Outcome::Invalid;
                }
                match &mut action {
                    Action::Join {
                        count: join_count, ..
                    } => *join_count = count,
                    Action::TabNext {
                        count: tab_count, ..
                    } => *tab_count = count,
                    _ => {}
                }
                return complete(action, count, register);
            }
            match parse_motion(keys, index) {
                MotionParse::Complete(motion, consumed) if index + consumed == keys.len() => {
                    Outcome::Complete(Command::Move { count, motion })
                }
                MotionParse::Complete(_, _) => Outcome::Invalid,
                MotionParse::Pending => Outcome::Pending,
                MotionParse::Invalid => Outcome::Invalid,
            }
        }
    }
}

fn complete(action: Action, count: usize, register: Option<char>) -> Outcome {
    Outcome::Complete(Command::Action {
        count,
        register,
        action,
    })
}

fn parse_count(keys: &[InputEvent]) -> Option<(usize, usize)> {
    let mut index = 0;
    let mut count = 0usize;
    while let Some(InputEvent::Char(character)) = keys.get(index).copied() {
        let Some(digit) = character.to_digit(10) else {
            break;
        };
        if count == 0 && digit == 0 {
            break;
        }
        count = count
            .saturating_mul(10)
            .saturating_add(digit as usize)
            .min(999_999_999);
        index += 1;
    }
    Some((count.max(1), index))
}

fn parse_operator(keys: &[InputEvent], index: usize) -> Option<(Operator, usize)> {
    let first = keys.get(index).copied()?;
    let operator = match first {
        InputEvent::Char('d') => (Operator::Delete, 1),
        InputEvent::Char('c') => (Operator::Change, 1),
        InputEvent::Char('y') => (Operator::Yank, 1),
        InputEvent::Char('>') => (Operator::ShiftRight, 1),
        InputEvent::Char('<') => (Operator::ShiftLeft, 1),
        InputEvent::Char('=') => (Operator::Reindent, 1),
        InputEvent::Char('g') => {
            let second = keys.get(index + 1).copied()?;
            match second {
                InputEvent::Char('~') => (Operator::ToggleCase, 2),
                InputEvent::Char('u') => (Operator::Lowercase, 2),
                InputEvent::Char('U') => (Operator::Uppercase, 2),
                InputEvent::Char('?') => (Operator::Rot13, 2),
                InputEvent::Char('q') => (Operator::Format, 2),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(operator)
}

fn repeats_operator(operator: Operator, next: InputEvent) -> bool {
    matches!(
        (operator, next),
        (Operator::Delete, InputEvent::Char('d'))
            | (Operator::Change, InputEvent::Char('c'))
            | (Operator::Yank, InputEvent::Char('y'))
            | (Operator::ShiftRight, InputEvent::Char('>'))
            | (Operator::ShiftLeft, InputEvent::Char('<'))
            | (Operator::Reindent, InputEvent::Char('='))
            | (Operator::ToggleCase, InputEvent::Char('~'))
            | (Operator::Lowercase, InputEvent::Char('u'))
            | (Operator::Uppercase, InputEvent::Char('U'))
            | (Operator::Rot13, InputEvent::Char('?'))
            | (Operator::Format, InputEvent::Char('q'))
    )
}

fn window_action(character: char) -> Option<WindowAction> {
    Some(match character {
        's' => WindowAction::SplitBelow,
        'v' => WindowAction::SplitRight,
        'w' | 'W' => WindowAction::Next,
        'p' | 'P' => WindowAction::Previous,
        'h' => WindowAction::Left,
        'l' => WindowAction::Right,
        'k' => WindowAction::Up,
        'j' => WindowAction::Down,
        'q' | 'c' => WindowAction::Close,
        'o' => WindowAction::Only,
        '=' => WindowAction::Equalize,
        '+' => WindowAction::Taller,
        '-' => WindowAction::Shorter,
        '>' => WindowAction::Wider,
        '<' => WindowAction::Narrower,
        'r' | 'R' => WindowAction::Rotate,
        _ => return None,
    })
}

fn parse_g_action(keys: &[InputEvent], index: usize) -> Option<(Action, usize)> {
    let first = keys.get(index).copied()?;
    if first != InputEvent::Char('g') {
        return None;
    }
    let second = keys.get(index + 1).copied()?;
    let action = match second {
        InputEvent::Char('h') => (Action::Select(SelectionMode::Charwise), 2),
        InputEvent::Char('H') => (Action::Select(SelectionMode::Linewise), 2),
        InputEvent::Char('v') => (Action::ReselectVisual, 2),
        InputEvent::Char('J') => (
            Action::Join {
                spaces: false,
                count: 1,
            },
            2,
        ),
        InputEvent::Char('I') => (Action::Insert(InsertEntry::FirstNonBlank), 2),
        InputEvent::Char('i') => (Action::Insert(InsertEntry::LastInsert), 2),
        InputEvent::Char('-') => (Action::UndoEarlier, 2),
        InputEvent::Char('+') => (Action::UndoLater, 2),
        InputEvent::Char('t') => (
            Action::TabNext {
                count: 1,
                forward: true,
            },
            2,
        ),
        InputEvent::Char('T') => (
            Action::TabNext {
                count: 1,
                forward: false,
            },
            2,
        ),
        _ => return None,
    };
    Some(action)
}

enum MotionParse {
    Complete(Motion, usize),
    Pending,
    Invalid,
}

fn parse_motion(keys: &[InputEvent], index: usize) -> MotionParse {
    let Some(event) = keys.get(index).copied() else {
        return MotionParse::Pending;
    };
    match event {
        InputEvent::Char('h') | InputEvent::Left => MotionParse::Complete(Motion::Left, 1),
        InputEvent::Char('l') | InputEvent::Right => MotionParse::Complete(Motion::Right, 1),
        InputEvent::Char('j') | InputEvent::Down => MotionParse::Complete(Motion::Down, 1),
        InputEvent::Char('k') | InputEvent::Up => MotionParse::Complete(Motion::Up, 1),
        InputEvent::Char('w') => MotionParse::Complete(Motion::WordForward { big: false }, 1),
        InputEvent::Char('W') => MotionParse::Complete(Motion::WordForward { big: true }, 1),
        InputEvent::Char('b') => MotionParse::Complete(Motion::WordBack { big: false }, 1),
        InputEvent::Char('B') => MotionParse::Complete(Motion::WordBack { big: true }, 1),
        InputEvent::Char('e') => MotionParse::Complete(Motion::WordEnd { big: false }, 1),
        InputEvent::Char('E') => MotionParse::Complete(Motion::WordEnd { big: true }, 1),
        InputEvent::Char('0') => MotionParse::Complete(Motion::LineStart, 1),
        InputEvent::Char('^') => MotionParse::Complete(Motion::FirstNonBlank, 1),
        InputEvent::Char('$') => MotionParse::Complete(Motion::LineEnd, 1),
        InputEvent::Char('|') => MotionParse::Complete(Motion::ScreenColumn, 1),
        InputEvent::Char('f' | 'F' | 't' | 'T') => {
            let InputEvent::Char(character) = event else {
                unreachable!();
            };
            let Some(InputEvent::Char(target)) = keys.get(index + 1).copied() else {
                return MotionParse::Pending;
            };
            MotionParse::Complete(
                Motion::Find {
                    character: target,
                    forward: matches!(character, 'f' | 't'),
                    until: matches!(character, 't' | 'T'),
                },
                2,
            )
        }
        InputEvent::Char(';') => MotionParse::Complete(Motion::RepeatFind { reverse: false }, 1),
        InputEvent::Char(',') => MotionParse::Complete(Motion::RepeatFind { reverse: true }, 1),
        InputEvent::Char('%') => MotionParse::Complete(Motion::MatchPair, 1),
        InputEvent::Char('{') => MotionParse::Complete(Motion::ParagraphBack, 1),
        InputEvent::Char('}') => MotionParse::Complete(Motion::ParagraphForward, 1),
        InputEvent::Char('G') => MotionParse::Complete(Motion::GotoLine, 1),
        InputEvent::Char('n') => MotionParse::Complete(Motion::SearchNext, 1),
        InputEvent::Char('N') => MotionParse::Complete(Motion::SearchPrevious, 1),
        InputEvent::Char('*') => {
            MotionParse::Complete(Motion::WordUnderCursor { forward: true }, 1)
        }
        InputEvent::Char('#') => {
            MotionParse::Complete(Motion::WordUnderCursor { forward: false }, 1)
        }
        InputEvent::Char('g') => match keys.get(index + 1).copied() {
            Some(InputEvent::Char('g')) => MotionParse::Complete(Motion::GotoFileStart, 2),
            Some(InputEvent::Char('e')) => {
                MotionParse::Complete(Motion::WordEndBack { big: false }, 2)
            }
            Some(InputEvent::Char('E')) => {
                MotionParse::Complete(Motion::WordEndBack { big: true }, 2)
            }
            Some(InputEvent::Char('_')) => MotionParse::Complete(Motion::LineEnd, 2),
            Some(_) => MotionParse::Invalid,
            None => MotionParse::Pending,
        },
        InputEvent::Char('i' | 'a') => MotionParse::Invalid,
        _ => MotionParse::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<InputEvent> {
        text.chars().map(InputEvent::Char).collect()
    }

    fn parse_text(text: &str) -> Outcome {
        parse(&chars(text))
    }

    #[test]
    fn counts_compose_multiplicatively() {
        assert_eq!(
            parse_text("3d2w"),
            Outcome::Complete(Command::Operator {
                count: 3,
                operator: Operator::Delete,
                register: None,
                target: Target::Motion {
                    count: 6,
                    motion: Motion::WordForward { big: false }
                }
            })
        );
        assert_eq!(
            parse_text("3dw"),
            Outcome::Complete(Command::Operator {
                count: 3,
                operator: Operator::Delete,
                register: None,
                target: Target::Motion {
                    count: 3,
                    motion: Motion::WordForward { big: false }
                }
            })
        );
    }

    #[test]
    fn register_prefixes_are_part_of_the_command() {
        assert_eq!(
            parse_text("\"a3dd"),
            Outcome::Complete(Command::Operator {
                count: 3,
                operator: Operator::Delete,
                register: Some('a'),
                target: Target::Line
            })
        );
        assert_eq!(
            parse_text("\"Ayy"),
            Outcome::Complete(Command::Operator {
                count: 1,
                operator: Operator::Yank,
                register: Some('A'),
                target: Target::Line
            })
        );
    }

    #[test]
    fn text_objects_and_special_motions_parse() {
        assert_eq!(
            parse_text("ci\""),
            Outcome::Complete(Command::Operator {
                count: 1,
                operator: Operator::Change,
                register: None,
                target: Target::TextObject {
                    count: 1,
                    around: false,
                    object: TextObject::Quote('"')
                }
            })
        );
        assert_eq!(
            parse_text("da("),
            Outcome::Complete(Command::Operator {
                count: 1,
                operator: Operator::Delete,
                register: None,
                target: Target::TextObject {
                    count: 1,
                    around: true,
                    object: TextObject::Pair('(')
                }
            })
        );
        assert!(matches!(
            parse_text("3gg"),
            Outcome::Complete(Command::Move {
                count: 3,
                motion: Motion::GotoFileStart
            })
        ));
    }

    #[test]
    fn incomplete_prefixes_stay_pending_until_resolved() {
        assert_eq!(parse_text("3"), Outcome::Pending);
        assert_eq!(parse_text("d"), Outcome::Pending);
        assert_eq!(parse_text("d2"), Outcome::Pending);
        assert_eq!(parse_text("f"), Outcome::Pending);
        assert_eq!(parse_text("\""), Outcome::Pending);
        assert_eq!(parse_text("g"), Outcome::Pending);
        assert_eq!(parse_text("q"), Outcome::Pending);
    }

    #[test]
    fn invalid_sequences_do_not_execute() {
        assert_eq!(parse_text("dz"), Outcome::Invalid);
        assert_eq!(parse_text("g?"), Outcome::Pending);
        assert!(matches!(parse_text("i!"), Outcome::Complete(_)));
    }

    #[test]
    fn parser_retains_a_pending_prefix_and_clears_on_completion() {
        let mut parser = Parser::default();
        assert_eq!(parser.feed(InputEvent::Char('3')), Outcome::Pending);
        assert!(parser.is_pending());
        assert_eq!(parser.feed(InputEvent::Char('d')), Outcome::Pending);
        assert_eq!(
            parser.feed(InputEvent::Char('w')),
            Outcome::Complete(Command::Operator {
                count: 3,
                operator: Operator::Delete,
                register: None,
                target: Target::Motion {
                    count: 3,
                    motion: Motion::WordForward { big: false }
                }
            })
        );
        assert!(!parser.is_pending());
    }
}
