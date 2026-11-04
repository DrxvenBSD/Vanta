//! Vim-style register storage.
//!
//! The bank keeps the unnamed pointer, yank register, delete history, named
//! registers, small-delete register and the read-only editor registers. The
//! platform clipboard is not guessed here: `+` and `*` are stored in-process
//! until a terminal clipboard backend is attached, so an explicit copy never
//! silently reaches an unrelated program.

use std::collections::BTreeMap;

/// How register text should be pasted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RegisterKind {
    /// Insert text at the cursor.
    #[default]
    Charwise,
    /// Insert whole lines.
    Linewise,
    /// Insert a rectangular block; width is measured in display cells.
    Blockwise {
        /// Rectangle width in cells.
        width: usize,
    },
}

/// One register entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisterValue {
    pub(crate) text: String,
    pub(crate) kind: RegisterKind,
}

impl RegisterValue {
    pub(crate) fn new(text: String, kind: RegisterKind) -> Self {
        Self { text, kind }
    }
}

/// How a write reached the register bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegisterWrite {
    Yank,
    Delete { small: bool },
    Change { small: bool },
}

/// The editor's register bank.
#[derive(Clone, Debug)]
pub(crate) struct Registers {
    slots: BTreeMap<char, RegisterValue>,
    unnamed: char,
    last_insert: String,
    last_command: String,
    last_search: String,
    current_file: Option<String>,
    alternate_file: Option<String>,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            slots: BTreeMap::new(),
            unnamed: '0',
            last_insert: String::new(),
            last_command: String::new(),
            last_search: String::new(),
            current_file: None,
            alternate_file: None,
        }
    }
}

impl Registers {
    /// Reads a register. `None` means the unnamed register.
    pub(crate) fn read(&self, name: Option<char>) -> Option<RegisterValue> {
        let name = match name {
            None | Some('"') => self.unnamed,
            Some(name @ 'A'..='Z') => name.to_ascii_lowercase(),
            Some(name) => name,
        };
        match name {
            '_' => None,
            '.' => Some(RegisterValue::new(
                self.last_insert.clone(),
                RegisterKind::Charwise,
            )),
            ':' => Some(RegisterValue::new(
                self.last_command.clone(),
                RegisterKind::Charwise,
            )),
            '/' => Some(RegisterValue::new(
                self.last_search.clone(),
                RegisterKind::Charwise,
            )),
            '%' => Some(RegisterValue::new(
                self.current_file.clone().unwrap_or_default(),
                RegisterKind::Charwise,
            )),
            '#' => Some(RegisterValue::new(
                self.alternate_file.clone().unwrap_or_default(),
                RegisterKind::Charwise,
            )),
            '+' | '*' => self.slots.get(&name).cloned(),
            _ => self.slots.get(&name).cloned(),
        }
    }

    /// Writes a yank.
    pub(crate) fn yank(&mut self, name: Option<char>, text: String, kind: RegisterKind) {
        self.write(name, text, kind, RegisterWrite::Yank);
    }

    /// Writes a delete, including numbered and small-delete semantics.
    pub(crate) fn delete(
        &mut self,
        name: Option<char>,
        text: String,
        kind: RegisterKind,
        small: bool,
    ) {
        self.write(name, text, kind, RegisterWrite::Delete { small });
    }

    /// Writes a change, including numbered and small-delete semantics.
    pub(crate) fn change(
        &mut self,
        name: Option<char>,
        text: String,
        kind: RegisterKind,
        small: bool,
    ) {
        self.write(name, text, kind, RegisterWrite::Change { small });
    }

    fn write(
        &mut self,
        name: Option<char>,
        text: String,
        kind: RegisterKind,
        write: RegisterWrite,
    ) {
        let explicit = matches!(name, Some(name) if name != '"');
        let Some(name) = name.or(Some('"')) else {
            return;
        };
        if name == '_' || name == '"' && write != RegisterWrite::Yank && text.is_empty() {
            return;
        }
        if explicit && matches!(name, '.' | ':' | '/' | '%' | '#') {
            return;
        }

        if explicit {
            self.write_explicit(name, &text, kind, write);
        } else {
            self.write_implicit(&text, kind, write);
        }
        if name != '_' {
            let target = if name == '"' {
                self.unnamed
            } else if name.is_ascii_uppercase() {
                name.to_ascii_lowercase()
            } else {
                name
            };
            self.unnamed = target;
        }
    }

    fn write_implicit(&mut self, text: &str, kind: RegisterKind, write: RegisterWrite) {
        let small = match write {
            RegisterWrite::Delete { small } | RegisterWrite::Change { small } => small,
            RegisterWrite::Yank => false,
        };
        if small && kind == RegisterKind::Charwise {
            self.slots
                .insert('-', RegisterValue::new(text.to_string(), kind));
            self.unnamed = '-';
            return;
        }
        match write {
            RegisterWrite::Yank => {
                self.slots
                    .insert('0', RegisterValue::new(text.to_string(), kind));
                self.unnamed = '0';
            }
            RegisterWrite::Delete { .. } | RegisterWrite::Change { .. } => {
                self.shift_numbered();
                self.slots
                    .insert('1', RegisterValue::new(text.to_string(), kind));
                self.unnamed = '1';
            }
        }
    }

    fn write_explicit(&mut self, name: char, text: &str, kind: RegisterKind, write: RegisterWrite) {
        if let 'A'..='Z' = name {
            let target = name.to_ascii_lowercase();
            let entry = self
                .slots
                .entry(target)
                .or_insert_with(|| RegisterValue::new(String::new(), kind));
            if matches!(entry.kind, RegisterKind::Linewise)
                && !entry.text.ends_with('\n')
                && !entry.text.ends_with("\r\n")
            {
                entry.text.push('\n');
            }
            entry.text.push_str(text);
            return;
        }
        let small = match write {
            RegisterWrite::Delete { small } | RegisterWrite::Change { small } => small,
            RegisterWrite::Yank => false,
        };
        if let '1'..='9' | '-' = name {
            if name == '-' && !small && !text.is_empty() {
                self.shift_numbered();
                self.slots
                    .insert('1', RegisterValue::new(text.to_string(), kind));
                return;
            }
            self.slots
                .insert(name, RegisterValue::new(text.to_string(), kind));
            return;
        }
        if name == '+' || name == '*' || name == '=' || name.is_ascii_lowercase() {
            self.slots
                .insert(name, RegisterValue::new(text.to_string(), kind));
        }
    }

    fn shift_numbered(&mut self) {
        for number in (1..9).rev() {
            let next = char::from_digit(number + 1, 10).unwrap_or('9');
            let current = char::from_digit(number, 10).unwrap_or('1');
            if let Some(value) = self.slots.get(&current).cloned() {
                self.slots.insert(next, value);
            } else {
                self.slots.remove(&next);
            }
        }
    }

    /// Records inserted text for `.` and the read-only `.` register.
    pub(crate) fn set_last_insert(&mut self, text: &str) {
        self.last_insert = text.to_string();
    }

    /// Records the last Ex command.
    pub(crate) fn set_last_command(&mut self, command: &str) {
        self.last_command = command.to_string();
    }

    /// Records the last search pattern.
    pub(crate) fn set_last_search(&mut self, pattern: &str) {
        self.last_search = pattern.to_string();
    }

    /// Updates the `%` and `#` filename registers.
    pub(crate) fn set_current_file(&mut self, current: Option<String>, alternate: Option<String>) {
        self.current_file = current;
        self.alternate_file = alternate;
    }

    /// Renders `:registers` output.
    pub(crate) fn listing(&self) -> String {
        let mut names = vec!['"', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '-'];
        names.extend('a'..='z');
        names.extend(['+', '*', '.', ':', '/']);
        let mut lines = Vec::new();
        for name in names {
            if let Some(value) = self.read(Some(name)) {
                if value.text.is_empty() {
                    continue;
                }
                let kind = match value.kind {
                    RegisterKind::Charwise => "c",
                    RegisterKind::Linewise => "l",
                    RegisterKind::Blockwise { .. } => "b",
                };
                lines.push(format!("\"{name}   {kind}  {}", value.text.escape_debug()));
            }
        }
        if lines.is_empty() {
            "--- Registers ---".to_string()
        } else {
            format!("--- Registers ---\n{}", lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yank_and_delete_follow_vim_numbered_rules() {
        let mut registers = Registers::default();
        registers.yank(None, "one".to_string(), RegisterKind::Charwise);
        assert_eq!(registers.read(None).expect("unnamed").text, "one");
        assert_eq!(registers.read(Some('0')).expect("yank").text, "one");

        registers.delete(None, "line\n".to_string(), RegisterKind::Linewise, false);
        assert_eq!(registers.read(None).expect("unnamed").text, "line\n");
        assert_eq!(registers.read(Some('1')).expect("delete").text, "line\n");
        assert_eq!(registers.read(Some('0')).expect("old yank").text, "one");

        registers.delete(None, "x".to_string(), RegisterKind::Charwise, true);
        assert_eq!(registers.read(None).expect("small").text, "x");
        assert_eq!(registers.read(Some('-')).expect("small slot").text, "x");
        assert_eq!(
            registers.read(Some('1')).expect("previous delete").text,
            "line\n"
        );
    }

    #[test]
    fn named_append_keeps_value_and_unnamed_pointer() {
        let mut registers = Registers::default();
        registers.yank(Some('a'), "one".to_string(), RegisterKind::Charwise);
        registers.yank(Some('A'), "two".to_string(), RegisterKind::Charwise);
        assert_eq!(registers.read(Some('a')).expect("named").text, "onetwo");
        assert_eq!(registers.read(None).expect("unnamed").text, "onetwo");
    }

    #[test]
    fn black_hole_discards_without_moving_unnamed() {
        let mut registers = Registers::default();
        registers.yank(None, "keep".to_string(), RegisterKind::Charwise);
        registers.yank(Some('_'), "discard".to_string(), RegisterKind::Charwise);
        assert_eq!(registers.read(None).expect("unnamed").text, "keep");
    }

    #[test]
    fn listing_reports_kind_and_contents() {
        let mut registers = Registers::default();
        registers.yank(None, "abc".to_string(), RegisterKind::Linewise);
        let listing = registers.listing();
        assert!(listing.contains("\"0   l  abc"), "{listing}");
    }
}
