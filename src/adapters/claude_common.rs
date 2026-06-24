//! Shared helpers for the two claude adapters -- the print-mode default
//! (`claude`) and the PTY drive (`claude-pty`). They map a requested permission
//! tier to the same native claude flags and report the same enforcement
//! classes; only the invocation mechanism differs.

use crate::args::Options;
use crate::harness::Harness;
use crate::policy::{Enforcement, Network, Perms};

/// Resolve the claude binary to spawn. `ANYAGENT_CLAUDE_BIN` overrides it (for
/// tests, or a cmux-style shim that would clobber our flags); a custom harness
/// already carries its own path.
pub fn resolve_bin(harness: &Harness) -> String {
    if matches!(harness, Harness::Claude | Harness::ClaudePty)
        && let Ok(b) = std::env::var("ANYAGENT_CLAUDE_BIN")
    {
        return b;
    }
    harness.bin().to_string()
}

/// Native claude flags for the requested permission tier: read-only is policy
/// (`--permission-mode plan`); write tiers use bypassPermissions
/// (`--dangerously-skip-permissions`), as does `--dangerously-skip-permissions`
/// requested directly.
pub fn perms_args(opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    if matches!(opts.perms, Some(Perms::ReadOnly)) {
        v.push("--permission-mode".to_string());
        v.push("plan".to_string());
    }
    let bypass = opts.skip_permissions
        || matches!(opts.perms, Some(Perms::WorkspaceWrite) | Some(Perms::Full));
    if bypass {
        v.push("--dangerously-skip-permissions".to_string());
    }
    v
}

/// The enforcement class claude achieves for a permission tier. claude has no
/// OS sandbox: read-only is policy-only, write tiers are unenforced.
pub fn perms_enforcement(perms: Perms) -> Enforcement {
    match perms {
        Perms::ReadOnly => Enforcement::AgentPolicy,
        Perms::WorkspaceWrite | Perms::Full => Enforcement::Unenforced,
    }
}

/// claude has no native network control.
pub fn network_enforcement(_perms: Option<Perms>, _network: Network) -> Enforcement {
    Enforcement::Unenforced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_is_plan_mode() {
        let opts = Options {
            perms: Some(Perms::ReadOnly),
            ..Options::default()
        };
        assert_eq!(perms_args(&opts), vec!["--permission-mode", "plan"]);
        assert_eq!(perms_enforcement(Perms::ReadOnly), Enforcement::AgentPolicy);
    }

    #[test]
    fn write_tiers_bypass() {
        let opts = Options {
            perms: Some(Perms::WorkspaceWrite),
            ..Options::default()
        };
        assert_eq!(perms_args(&opts), vec!["--dangerously-skip-permissions"]);
        assert_eq!(perms_enforcement(Perms::Full), Enforcement::Unenforced);
    }

    #[test]
    fn skip_permissions_flag_bypasses() {
        let opts = Options {
            skip_permissions: true,
            ..Options::default()
        };
        assert_eq!(perms_args(&opts), vec!["--dangerously-skip-permissions"]);
    }
}
