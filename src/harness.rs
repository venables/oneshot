//! Selection of which agent CLI ("harness") anyagent drives.
//!
//! anyagent aims to be one non-interactive interface in front of any coding
//! agent. Today only the Claude protocol is implemented -- spawning the
//! interactive TUI under a PTY, injecting a Stop hook via `--settings`, and
//! capturing the final assistant message. The other names below are recognised
//! and reserved so the `--harness` surface is stable as backends are added;
//! selecting one that isn't wired up yet fails fast with a clear message
//! (see `is_supported`).
//!
//! A value that is not a known name is treated as a path/binary and driven with
//! the Claude protocol, so a fork or wrapper of `claude` can be pointed at
//! directly (this subsumes the `ANYAGENT_CLAUDE_BIN` escape hatch).

/// Known harness names, in the order shown in help/error text.
pub const KNOWN_NAMES: &[&str] = &["claude", "codex", "opencode", "gemini", "pi"];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Harness {
    #[default]
    Claude,
    Codex,
    Opencode,
    Gemini,
    Pi,
    /// A binary name or path not in the known list, driven with the Claude
    /// protocol (it must be a claude-compatible CLI).
    Custom(String),
}

impl Harness {
    /// Resolve a `--harness` value. Known names match case-insensitively;
    /// anything else is taken as a custom binary path.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "opencode" => Self::Opencode,
            "gemini" => Self::Gemini,
            "pi" => Self::Pi,
            _ => Self::Custom(s.to_string()),
        }
    }

    /// Human-facing name for diagnostics.
    pub fn name(&self) -> &str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
            Self::Custom(s) => s,
        }
    }

    /// The binary anyagent spawns for this harness.
    pub fn bin(&self) -> &str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
            Self::Custom(s) => s,
        }
    }

    /// Whether anyagent can drive this harness today. Only the Claude protocol
    /// is implemented; a custom binary is assumed claude-compatible.
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Claude | Self::Custom(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_claude() {
        assert_eq!(Harness::default(), Harness::Claude);
    }

    #[test]
    fn parses_known_names_case_insensitively() {
        assert_eq!(Harness::parse("claude"), Harness::Claude);
        assert_eq!(Harness::parse("Codex"), Harness::Codex);
        assert_eq!(Harness::parse("OPENCODE"), Harness::Opencode);
        assert_eq!(Harness::parse("gemini"), Harness::Gemini);
        assert_eq!(Harness::parse("pi"), Harness::Pi);
    }

    #[test]
    fn unknown_value_is_custom_path_preserving_case() {
        assert_eq!(
            Harness::parse("/opt/bin/My-Claude"),
            Harness::Custom("/opt/bin/My-Claude".to_string())
        );
    }

    #[test]
    fn only_claude_and_custom_supported_today() {
        assert!(Harness::Claude.is_supported());
        assert!(Harness::Custom("./claude".into()).is_supported());
        assert!(!Harness::Codex.is_supported());
        assert!(!Harness::Opencode.is_supported());
        assert!(!Harness::Gemini.is_supported());
        assert!(!Harness::Pi.is_supported());
    }

    #[test]
    fn bin_matches_name_for_known_harnesses() {
        for n in KNOWN_NAMES {
            assert_eq!(Harness::parse(n).bin(), *n);
        }
    }
}
