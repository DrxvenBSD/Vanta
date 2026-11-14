//! Terminal capability detection at the editor boundary.
//!
//! Capabilities are resolved from an active terminal profile plus documented
//! environment hints. The profile asks the OS/crossterm whether the process is
//! attached to a terminal, whether ANSI sequences are supported and how many
//! colors the terminal advertises. It does not assume a terminal name, and a
//! pipe or CI process resolves to the monochrome/no-ANSI fallback without
//! needing an interactive terminal.
//!
//! Detection is nonblocking. A terminal that cannot answer any part of the
//! profile falls back to the safe 16-color/no-scroll-region path; the first
//! frame is never held for a capability handshake.

use std::env;
use std::io::IsTerminal;

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
    /// Whether the text area may use DECSTBM scroll regions.
    pub(crate) scroll_regions: bool,
    /// Whether the backend may switch to the alternate screen.
    pub(crate) alternate_screen: bool,
}

/// The actively queried facts about the attached terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalProfile {
    is_terminal: bool,
    supports_ansi: bool,
    color_count: u16,
}

impl TerminalProfile {
    /// Queries the process's actual terminal attachment and advertised support.
    ///
    /// Non-TTY processes skip the platform probes and resolve to a degraded
    /// profile. This keeps capability tests and CI runs independent of an
    /// interactive terminal.
    fn detect() -> Self {
        let is_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if !is_terminal {
            return Self {
                is_terminal: false,
                supports_ansi: false,
                color_count: 0,
            };
        }
        Self {
            is_terminal,
            supports_ansi: ansi_support(),
            color_count: crossterm::style::available_color_count(),
        }
    }
}

/// Whether the attached console advertises ANSI sequence support.
///
/// crossterm 0.29 only ships this probe for the Windows console API, so the
/// call site is behind a platform function instead of a `cfg` branch in the
/// detection logic.
#[cfg(windows)]
fn ansi_support() -> bool {
    crossterm::ansi_support::supports_ansi()
}

/// Whether the attached console advertises ANSI sequence support.
///
/// A Unix process that reached `detect` is already known to own a TTY, and
/// `TERM=dumb` and `NO_COLOR` are resolved by `from_profile`, so there is no
/// platform-specific advertisement to query.
#[cfg(not(windows))]
fn ansi_support() -> bool {
    true
}

impl Capabilities {
    /// Detects capabilities from the active terminal profile and environment.
    pub(crate) fn detect() -> Self {
        Self::from_profile(TerminalProfile::detect(), |name| env::var(name).ok())
    }

    /// Resolves capabilities from an injected profile and lookup.
    ///
    /// This is the test seam. It is pure, so supported and degraded terminal
    /// paths can be tested without opening a pseudo-terminal.
    fn from_profile<F>(profile: TerminalProfile, lookup: F) -> Self
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

        let truecolor_hint = colorterm.contains("truecolor")
            || colorterm.contains("24bit")
            || term.contains("direct")
            || term.contains("kitty")
            || term.contains("wezterm")
            || term.contains("ghostty")
            || program.contains("kitty")
            || program.contains("wezterm")
            || program.contains("ghostty")
            || windows_terminal;

        // This is the active color-count probe. Unknown ANSI terminals still
        // get the 16-color fallback; a non-TTY or non-ANSI process gets none.
        let color = if no_color || !profile.is_terminal || !profile.supports_ansi {
            ColorDepth::None
        } else if truecolor_hint || profile.color_count == u16::MAX {
            ColorDepth::TrueColor
        } else if profile.color_count >= 256 {
            ColorDepth::Ansi256
        } else {
            ColorDepth::Ansi16
        };

        let ansi = profile.is_terminal && profile.supports_ansi;
        let known_synchronized = term.contains("kitty")
            || term.contains("wezterm")
            || term.contains("ghostty")
            || term.contains("foot")
            || program.contains("wezterm")
            || program.contains("ghostty")
            || program.contains("iterm")
            || windows_terminal;
        let synchronized_output =
            ansi && flag("VANTA_SYNCHRONIZED_OUTPUT").unwrap_or(known_synchronized);

        Self {
            color,
            synchronized_output,
            scroll_regions: ansi,
            alternate_screen: ansi,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn profile(is_terminal: bool, supports_ansi: bool, color_count: u16) -> TerminalProfile {
        TerminalProfile {
            is_terminal,
            supports_ansi,
            color_count,
        }
    }

    fn capabilities(profile: TerminalProfile, values: &[(&str, &str)]) -> Capabilities {
        let values: HashMap<String, String> = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        Capabilities::from_profile(profile, |name| values.get(name).cloned())
    }

    #[test]
    fn a_supported_truecolor_terminal_enables_the_full_rendering_path() {
        let detected = capabilities(
            profile(true, true, 256),
            &[
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("WT_SESSION", "session"),
            ],
        );
        assert_eq!(detected.color, ColorDepth::TrueColor);
        assert!(detected.synchronized_output);
        assert!(detected.scroll_regions);
        assert!(detected.alternate_screen);
    }

    #[test]
    fn the_active_color_count_selects_the_indexed_palette() {
        let detected = capabilities(profile(true, true, 256), &[("TERM", "vt100")]);
        assert_eq!(detected.color, ColorDepth::Ansi256);
        assert!(detected.scroll_regions);
    }

    #[test]
    fn the_active_color_count_detects_truecolor_without_environment_hints() {
        let detected = capabilities(profile(true, true, u16::MAX), &[("TERM", "vt100")]);
        assert_eq!(detected.color, ColorDepth::TrueColor);
    }

    #[test]
    fn a_degraded_terminal_disables_colors_alternate_screen_and_scroll_regions() {
        let detected = capabilities(profile(false, false, 0), &[("TERM", "xterm-256color")]);
        assert_eq!(
            detected,
            Capabilities {
                color: ColorDepth::None,
                synchronized_output: false,
                scroll_regions: false,
                alternate_screen: false,
            }
        );
    }

    #[test]
    fn no_color_and_dumb_terminals_disable_foreground_color() {
        assert_eq!(
            capabilities(
                profile(true, true, 256),
                &[("NO_COLOR", "1"), ("TERM", "xterm-256color")]
            )
            .color,
            ColorDepth::None
        );
        assert_eq!(
            capabilities(
                profile(true, true, 256),
                &[("TERM", "dumb"), ("COLORTERM", "truecolor")]
            )
            .color,
            ColorDepth::None
        );
    }

    #[test]
    fn synchronized_output_has_an_explicit_override() {
        assert!(
            capabilities(
                profile(true, true, 16),
                &[
                    ("TERM", "xterm-256color"),
                    ("VANTA_SYNCHRONIZED_OUTPUT", "1")
                ]
            )
            .synchronized_output
        );
        assert!(
            !capabilities(
                profile(true, true, 16),
                &[("TERM", "xterm-kitty"), ("VANTA_SYNCHRONIZED_OUTPUT", "0")]
            )
            .synchronized_output
        );
    }

    #[test]
    fn an_ansi_terminal_without_a_color_count_uses_the_16_color_fallback() {
        let detected = capabilities(profile(true, true, 0), &[("TERM", "vt100")]);
        assert_eq!(detected.color, ColorDepth::Ansi16);
        assert!(detected.scroll_regions);
        assert!(detected.alternate_screen);
        assert!(!detected.synchronized_output);
    }
}
