//! Terminal capability detection at the editor boundary.
//!
//! Detection is deliberately environment-based and synchronous: it performs no
//! terminal query and therefore cannot delay the first frame. A query-based
//! upgrade can be added behind a bounded timeout when the input loop supports
//! it. Unknown terminals fall back to 16 ANSI colors and no synchronized
//! output, which is safe for every supported platform.

use std::env;

/// Color depth the terminal can display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorDepth {
    /// No foreground colors; attributes remain available.
    None,
    /// The standard 16 ANSI colors.
    Ansi16,
    /// The 256-color indexed palette.
    Ansi256,
    /// 24-bit RGB color.
    TrueColor,
}

/// Capabilities negotiated once when the terminal is entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// Foreground color depth used by semantic styles.
    pub(crate) color: ColorDepth,
    /// Whether frames should be bracketed by DEC private mode 2026.
    pub(crate) synchronized_output: bool,
}

impl Capabilities {
    /// Detects capabilities from the process environment without blocking.
    pub(crate) fn detect() -> Self {
        Self::from_lookup(|name| env::var(name).ok())
    }

    /// Detects capabilities through an injected lookup function.
    ///
    /// The lookup returns `None` for an unset variable. This is the test seam;
    /// [`Capabilities::detect`] supplies the process environment.
    pub(crate) fn from_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let value = |name: &str| lookup(name).unwrap_or_default();
        let flag = |name: &str| match lookup(name).as_deref() {
            Some("1" | "true" | "TRUE" | "yes" | "YES") => Some(true),
            Some("0" | "false" | "FALSE" | "no" | "NO") => Some(false),
            _ => None,
        };

        let term = value("TERM").to_ascii_lowercase();
        let colorterm = value("COLORTERM").to_ascii_lowercase();
        let program = value("TERM_PROGRAM").to_ascii_lowercase();
        let windows_terminal = lookup("WT_SESSION").is_some();

        let no_color = lookup("NO_COLOR").is_some_and(|value| !value.is_empty()) || term == "dumb";

        let truecolor = colorterm.contains("truecolor")
            || colorterm.contains("24bit")
            || term.contains("direct")
            || term.contains("kitty")
            || term.contains("wezterm")
            || term.contains("ghostty")
            || program.contains("kitty")
            || program.contains("wezterm")
            || program.contains("ghostty")
            || windows_terminal;

        let ansi256 = term.contains("256color")
            || term.contains("256-colour")
            || program.contains("iterm")
            || program.contains("apple_terminal");

        let color = if no_color {
            ColorDepth::None
        } else if truecolor {
            ColorDepth::TrueColor
        } else if ansi256 {
            ColorDepth::Ansi256
        } else {
            ColorDepth::Ansi16
        };

        let known_synchronized = term.contains("kitty")
            || term.contains("wezterm")
            || term.contains("ghostty")
            || term.contains("foot")
            || program.contains("wezterm")
            || program.contains("ghostty")
            || program.contains("iterm")
            || windows_terminal;
        let synchronized_output = flag("VANTA_SYNCHRONIZED_OUTPUT").unwrap_or(known_synchronized);

        Self {
            color,
            synchronized_output,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn capabilities(values: &[(&str, &str)]) -> Capabilities {
        let values: HashMap<String, String> = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        Capabilities::from_lookup(|name| values.get(name).cloned())
    }

    #[test]
    fn truecolor_wins_over_a_256_color_term() {
        let detected = capabilities(&[
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("WT_SESSION", "session"),
        ]);
        assert_eq!(detected.color, ColorDepth::TrueColor);
        assert!(detected.synchronized_output);
    }

    #[test]
    fn a_256_color_term_falls_back_to_the_indexed_palette() {
        let detected = capabilities(&[("TERM", "xterm-256color")]);
        assert_eq!(detected.color, ColorDepth::Ansi256);
        assert!(!detected.synchronized_output);
    }

    #[test]
    fn no_color_and_dumb_terminals_disable_foreground_color() {
        assert_eq!(
            capabilities(&[("NO_COLOR", "1"), ("TERM", "xterm-256color")]).color,
            ColorDepth::None
        );
        assert_eq!(
            capabilities(&[("TERM", "dumb"), ("COLORTERM", "truecolor")]).color,
            ColorDepth::None
        );
    }

    #[test]
    fn synchronized_output_has_an_explicit_override() {
        assert!(
            capabilities(&[
                ("TERM", "xterm-256color"),
                ("VANTA_SYNCHRONIZED_OUTPUT", "1")
            ])
            .synchronized_output
        );
        assert!(
            !capabilities(&[("TERM", "xterm-kitty"), ("VANTA_SYNCHRONIZED_OUTPUT", "0")])
                .synchronized_output
        );
    }

    #[test]
    fn unknown_terminals_use_the_safe_ansi16_fallback() {
        assert_eq!(
            capabilities(&[("TERM", "vt100")]),
            Capabilities {
                color: ColorDepth::Ansi16,
                synchronized_output: false
            }
        );
    }
}
