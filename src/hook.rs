//! Stop/SessionStart hook plumbing for a `claude` invocation: a per-run temp
//! dir, a FIFO the parent reads, a tiny relay shell script that forwards the
//! hook payload to the FIFO, and the inline `--settings` JSON that registers
//! it. We never touch the user's `~/.claude/`.
//!
//! Lifetime: `HookHarness` cleans up its temp dir on `Drop`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use nix::sys::stat::Mode;
use nix::unistd;

const SCRIPT_BODY: &str = "\
#!/bin/sh
# Relay a Claude Code hook event to claude-p's FIFO.
#   $1 = event name (e.g. \"Stop\", \"SessionStart\")
# stdin = the hook's JSON payload (single line, no embedded newlines).
set -eu
event=\"$1\"
fifo=\"${CLAUDE_P_FIFO:?missing CLAUDE_P_FIFO}\"
payload=\"$(cat)\"
printf '%s\\t%s\\n' \"$event\" \"$payload\" >> \"$fifo\"
exit 0
";

pub struct HookHarness {
    pub tmp_dir: PathBuf,
    pub fifo_path: PathBuf,
    pub script_path: PathBuf,
    pub settings_json: String,
}

impl HookHarness {
    pub fn create() -> std::io::Result<Self> {
        let root = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp_dir = root.join(format!("claude-p-{pid}-{nanos:x}"));
        fs::create_dir_all(&tmp_dir)?;

        let fifo_path = tmp_dir.join("events.fifo");
        let script_path = tmp_dir.join("hook.sh");

        unistd::mkfifo(&fifo_path, Mode::from_bits_truncate(0o600))
            .map_err(|e| std::io::Error::other(format!("mkfifo: {e}")))?;

        let mut script = fs::File::create(&script_path)?;
        script.write_all(SCRIPT_BODY.as_bytes())?;
        let mut perms = script.metadata()?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&script_path, perms)?;
        drop(script);

        let settings_json = build_settings_json(&script_path.to_string_lossy());

        Ok(Self {
            tmp_dir,
            fifo_path,
            script_path,
            settings_json,
        })
    }
}

impl Drop for HookHarness {
    fn drop(&mut self) {
        // Best-effort cleanup.
        let _ = fs::remove_file(&self.fifo_path);
        let _ = fs::remove_file(&self.script_path);
        let _ = fs::remove_dir(&self.tmp_dir);
    }
}

fn build_settings_json(script_path: &str) -> String {
    // SessionStart (UI-ready signal, carries transcript_path) and Stop (turn
    // finished). The relay script takes the event name as $1.
    let event = |name: &str| {
        serde_json::json!([{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": format!("{script_path} {name}"),
            }],
        }])
    };
    serde_json::json!({
        "hooks": {
            "SessionStart": event("SessionStart"),
            "Stop": event("Stop"),
        }
    })
    .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    Stop,
    Unknown,
}

impl HookEvent {
    fn from_str(s: &str) -> Self {
        match s {
            "SessionStart" => Self::SessionStart,
            "Stop" => Self::Stop,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookLine {
    pub event: HookEvent,
    pub payload: String,
}

/// Parse one relay line of the form `<event>\t<json>`. Trailing CR/LF tolerated.
pub fn parse_line(raw: &str) -> Option<HookLine> {
    let line = raw.trim_end_matches('\n').trim_end_matches('\r');
    let tab = line.find('\t')?;
    Some(HookLine {
        event: HookEvent::from_str(&line[..tab]),
        payload: line[tab + 1..].to_string(),
    })
}

/// Fields pulled from a hook payload in a single parse pass.
#[derive(Debug, Default)]
pub struct PayloadFields {
    pub transcript_path: Option<String>,
    pub last_assistant_message: Option<String>,
    pub session_id: Option<String>,
}

/// Extract the fields we care about from a hook payload JSON in one pass
/// (the original parsed the same payload three separate times).
pub fn extract_fields(payload_json: &str) -> PayloadFields {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return PayloadFields::default();
    };
    let s = |k: &str| {
        value
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    PayloadFields {
        transcript_path: s("transcript_path"),
        last_assistant_message: s("last_assistant_message"),
        session_id: s("session_id"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_well_formed_with_both_events() {
        let json = build_settings_json("/tmp/hook.sh");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let hooks = &v["hooks"];
        assert!(hooks.get("SessionStart").is_some());
        assert!(hooks.get("Stop").is_some());
        let stop_cmd = hooks["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(stop_cmd.contains("/tmp/hook.sh"));
        assert!(stop_cmd.ends_with(" Stop"));
        assert_eq!(hooks["Stop"][0]["matcher"], "*");
    }

    #[test]
    fn parse_line_well_formed() {
        let l = parse_line("Stop\t{\"transcript_path\":\"/tmp/x.jsonl\"}\n").unwrap();
        assert_eq!(l.event, HookEvent::Stop);
        assert_eq!(l.payload, "{\"transcript_path\":\"/tmp/x.jsonl\"}");
    }

    #[test]
    fn parse_line_unknown_event() {
        let l = parse_line("PreFooBar\t{}").unwrap();
        assert_eq!(l.event, HookEvent::Unknown);
    }

    #[test]
    fn parse_line_no_tab_is_none() {
        assert!(parse_line("nope-no-tab").is_none());
    }

    #[test]
    fn extract_fields_single_pass() {
        let f = extract_fields(
            "{\"transcript_path\":\"/a/b.jsonl\",\"last_assistant_message\":\"OK\",\"session_id\":\"x\"}",
        );
        assert_eq!(f.transcript_path.as_deref(), Some("/a/b.jsonl"));
        assert_eq!(f.last_assistant_message.as_deref(), Some("OK"));
        assert_eq!(f.session_id.as_deref(), Some("x"));
    }

    #[test]
    fn extract_fields_tolerates_garbage() {
        let f = extract_fields("not json");
        assert!(f.transcript_path.is_none());
    }

    #[test]
    fn create_and_drop_round_trip() {
        let h = HookHarness::create().unwrap();
        assert!(h.script_path.exists());
        assert!(h.fifo_path.exists());
        let dir = h.tmp_dir.clone();
        drop(h);
        assert!(!dir.exists());
    }
}
