//! Codex adapter: drive `codex exec`, which is natively non-interactive, so
//! there is none of the PTY/hook/DEC machinery the claude adapter needs. We
//! spawn a plain subprocess, pipe the prompt on stdin (no positional argument,
//! so a prompt beginning with `-` is never mistaken for a flag), and read the
//! `--json` event stream on stdout.
//!
//! The event stream exposes the answer (`item.completed` / `agent_message`),
//! token usage (`turn.completed`), and the session id (`thread.started`), but
//! *not* the model. For an honest `model_resolved` we read codex's own session
//! rollout file (`turn_context.payload.model`) keyed by that session id -- the
//! launcher's truth -- and fall back to `"unknown"` rather than echoing the
//! requested model.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapters::procgroup;
use crate::adapters::{Adapter, DriverError, RunOutcome};
use crate::args::Options;
use crate::policy::{Enforcement, Network, Perms};
use crate::signals;
use crate::transcript::{Summary, Usage};

const POLL: Duration = Duration::from_millis(50);
/// Cap on captured stderr surfaced when codex fails without a JSON error event.
const STDERR_TAIL_CAP: usize = 8192;
/// Bounded wait for the detached stderr reader to drain before snapshotting the
/// tail for a failure diagnostic -- long enough to catch a fast startup/auth
/// failure, short enough to never stall a real run.
const STDERR_DRAIN_WAIT: Duration = Duration::from_millis(200);

/// Drives the `codex` CLI via its non-interactive `exec` subcommand.
pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn run(
        &self,
        opts: &Options,
        stream_out: Option<&mut dyn Write>,
    ) -> Result<RunOutcome, DriverError> {
        run(opts, stream_out)
    }

    fn drive(&self) -> &'static str {
        "exec"
    }

    fn perms_enforcement(&self, perms: Perms) -> Enforcement {
        match perms {
            // codex `--sandbox read-only|workspace-write` is an OS sandbox.
            Perms::ReadOnly | Perms::WorkspaceWrite => Enforcement::OsSandbox,
            // danger-full-access removes the sandbox.
            Perms::Full => Enforcement::Unenforced,
        }
    }

    fn network_enforcement(&self, perms: Option<Perms>, network: Network) -> Enforcement {
        match network {
            // codex's read-only/workspace-write sandboxes disable network by
            // default, so requesting no network is OS-enforced there.
            Network::None => match perms {
                Some(Perms::ReadOnly) | Some(Perms::WorkspaceWrite) => Enforcement::OsSandbox,
                _ => Enforcement::Unenforced,
            },
            // We don't open the sandbox's network back up, so anything other
            // than "none" is best-effort passthrough.
            Network::Restricted | Network::Full => Enforcement::Unenforced,
        }
    }
}

/// Accumulated state folded from the codex `--json` event stream.
#[derive(Debug, Default, PartialEq)]
struct Folded {
    final_text: String,
    session_id: String,
    usage: Usage,
    num_turns: u32,
    is_error: bool,
    /// The harness's own error text, surfaced to the user on failure.
    error_message: String,
    /// The error looks like the harness rejecting the requested model.
    invalid_model: bool,
}

/// Heuristic: does a harness error message indicate the model was rejected?
/// Matched against codex's own error text (e.g. "The 'x' model is not
/// supported ..."), so exit 31 reflects the harness's live verdict.
fn looks_like_model_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("model")
        && (m.contains("not supported")
            || m.contains("not found")
            || m.contains("does not exist")
            || m.contains("unknown model")
            || m.contains("invalid model"))
}

/// Fold one event line into the running state. Unknown lines are ignored, so a
/// new codex event type never breaks the parse.
fn fold_event(state: &mut Folded, line: &str) {
    let Ok(obj) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(ty) = obj.get("type").and_then(Value::as_str) else {
        return;
    };
    match ty {
        "thread.started" => {
            if let Some(id) = obj.get("thread_id").and_then(Value::as_str) {
                state.session_id = id.to_string();
            }
        }
        "item.completed" => {
            let item = obj.get("item");
            let is_message = item
                .and_then(|i| i.get("type"))
                .and_then(Value::as_str)
                == Some("agent_message");
            if is_message
                && let Some(text) = item.and_then(|i| i.get("text")).and_then(Value::as_str)
            {
                // Last agent message wins, matching the claude adapter.
                state.final_text = text.to_string();
            }
        }
        "turn.completed" => {
            state.num_turns += 1;
            if let Some(u) = obj.get("usage") {
                let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                state.usage = Usage {
                    input_tokens: get("input_tokens"),
                    output_tokens: get("output_tokens"),
                    cache_read_input_tokens: get("cached_input_tokens"),
                    // Codex has no cache-creation counter.
                    cache_creation_input_tokens: 0,
                };
            }
        }
        // Any failure/error event marks the run as errored.
        t if t.contains("failed") || t == "error" => {
            state.is_error = true;
            let msg = obj
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| {
                    obj.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default();
            if !msg.is_empty() {
                state.error_message = msg.to_string();
            }
            if looks_like_model_error(msg) {
                state.invalid_model = true;
            }
        }
        _ => {}
    }
}

/// Extract the resolved model from a codex session rollout file: the last
/// `turn_context` event's `payload.model`. `None` if absent or unparseable.
fn model_from_rollout(contents: &str) -> Option<String> {
    let mut model = None;
    for line in contents.lines() {
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) == Some("turn_context")
            && let Some(m) = obj
                .get("payload")
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str)
        {
            model = Some(m.to_string());
        }
    }
    model
}

/// `$CODEX_HOME` or `~/.codex`.
fn codex_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("CODEX_HOME") {
        return Some(PathBuf::from(h));
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".codex"))
}

/// Best-effort: the configured default model from `$CODEX_HOME/config.toml`.
/// codex has no model-enumeration command, so this is the most we can probe
/// without making a paid call.
pub fn configured_model() -> Option<String> {
    let contents = std::fs::read_to_string(codex_home()?.join("config.toml")).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("model")
            && let Some(val) = rest.trim_start().strip_prefix('=')
        {
            return Some(val.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Best-effort lookup of the resolved model from the rollout file whose name
/// contains `session_id`, under `$CODEX_HOME/sessions`.
fn resolve_model(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let sessions = codex_home()?.join("sessions");
    let path = find_rollout(&sessions, session_id)?;
    let contents = std::fs::read_to_string(path).ok()?;
    model_from_rollout(&contents)
}

/// Walk `$CODEX_HOME/sessions/**` for a `.jsonl` file whose name contains the
/// session id. Codex lays sessions out under year/month/day directories.
fn find_rollout(dir: &std::path::Path, session_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_rollout(&path, session_id) {
                return Some(found);
            }
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.contains(session_id)
            && name.ends_with(".jsonl")
        {
            return Some(path);
        }
    }
    None
}

fn build_argv(opts: &Options) -> Vec<String> {
    let mut v = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
        "--color".to_string(),
        "never".to_string(),
    ];
    if let Some(m) = &opts.model {
        v.push("--model".to_string());
        v.push(m.clone());
    }
    if let Some(cwd) = &opts.cwd {
        v.push("--cd".to_string());
        v.push(cwd.clone());
    }
    // Map the requested permission tier to codex's native OS sandbox.
    match opts.perms {
        Some(Perms::ReadOnly) => {
            v.push("--sandbox".to_string());
            v.push("read-only".to_string());
        }
        Some(Perms::WorkspaceWrite) => {
            v.push("--sandbox".to_string());
            v.push("workspace-write".to_string());
        }
        Some(Perms::Full) => {
            v.push("--sandbox".to_string());
            v.push("danger-full-access".to_string());
        }
        None => {}
    }
    if opts.skip_permissions {
        v.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    }
    v
}

fn run(opts: &Options, mut stream_out: Option<&mut dyn Write>) -> Result<RunOutcome, DriverError> {
    let start = Instant::now();
    let timeout = Duration::from_millis(opts.timeout_ms);

    // Codex floods stderr with its own diagnostics (skill-load errors, MCP
    // worker failures, "Reading prompt from stdin..."). That noise would drown
    // the run, so we don't pass it through by default -- but we do capture the
    // tail so a startup/auth failure that never reaches the JSON stdout stream
    // still yields a diagnostic instead of an empty answer. `--debug` also
    // mirrors it to our stderr live.
    let mut child = {
        let mut cmd = Command::new("codex");
        cmd.args(build_argv(opts))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // codex exec spawns its own tool subprocesses; lead a process group so
        // an interrupt/timeout tears the whole tree down, not just top-level.
        procgroup::lead_process_group(&mut cmd);
        cmd.spawn().map_err(|e| DriverError::Spawn(e.into()))?
    };

    // Feed the prompt on stdin from its own thread, then close it (dropping the
    // handle) so codex starts the turn. Writing on a dedicated thread avoids a
    // deadlock when the prompt exceeds the pipe buffer and codex has started
    // writing stdout before draining stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = opts.prompt.clone();
        thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
        });
    }

    // Drain stderr into a bounded byte tail (and mirror to our stderr under
    // --debug) so it can't fill the pipe and block codex, while preserving the
    // last bytes for a failure diagnostic. Read in fixed-size chunks (not
    // read_until) so a newline-free flood can't buffer an unbounded line before
    // the cap applies. Kept as raw bytes -- a mid-UTF-8 truncation or stray
    // non-UTF-8 byte must not panic or abort the drain; we lossily decode only
    // when surfacing. The thread is detached (never joined): a tool descendant
    // that inherits stderr and outlives codex could otherwise hang a join
    // forever, so we snapshot the shared tail after a bounded readiness wait.
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let debug = opts.debug;
    let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_done = Arc::new(AtomicBool::new(false));
    let stderr_tail_writer = Arc::clone(&stderr_tail);
    let stderr_done_writer = Arc::clone(&stderr_done);
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stderr_pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if debug {
                        let _ = std::io::stderr().write_all(&chunk[..n]);
                    }
                    if let Ok(mut tail) = stderr_tail_writer.lock() {
                        tail.extend_from_slice(&chunk[..n]);
                        if tail.len() > STDERR_TAIL_CAP {
                            let cut = tail.len() - STDERR_TAIL_CAP;
                            tail.drain(..cut);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        stderr_done_writer.store(true, Ordering::SeqCst);
    });

    // Read stdout lines on a thread so the main loop can honor the timeout and
    // interrupts even while a read would otherwise block.
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let streaming = opts.output_format == crate::args::OutputFormat::StreamJson
        && stream_out.is_some();
    let mut folded = Folded::default();
    let mut replay = String::new();

    loop {
        if signals::interrupted() {
            procgroup::terminate_group(child.id());
            let _ = child.wait();
            let _ = reader.join();
            return Err(DriverError::Interrupted);
        }
        if start.elapsed() > timeout {
            procgroup::terminate_group(child.id());
            let _ = child.wait();
            let _ = reader.join();
            return Err(DriverError::StopTimeout);
        }
        match rx.recv_timeout(POLL) {
            Ok(line) => {
                if streaming
                    && let Some(w) = stream_out.as_mut()
                {
                    let _ = w.write_all(line.as_bytes());
                    let _ = w.flush();
                }
                replay.push_str(&line);
                fold_event(&mut folded, line.trim_end());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = reader.join();
    let status = child.wait().map_err(DriverError::Io)?;
    if !status.success() {
        folded.is_error = true;
    }

    let model = resolve_model(&folded.session_id).unwrap_or_default();
    let duration_ms = start.elapsed().as_millis() as u64;

    // On failure with no agent message, surface the harness's own error text so
    // the user sees why (e.g. an unsupported-model message) instead of nothing.
    // Prefer a JSON `error` event; if there is none (a failure before codex
    // emitted any structured event -- auth, sandbox, startup), fall back to the
    // captured stderr tail rather than returning an empty answer.
    let final_text = if !folded.final_text.is_empty() {
        folded.final_text
    } else if !folded.error_message.is_empty() {
        folded.error_message
    } else if folded.is_error {
        // A startup/auth failure emits no JSON event, so nothing else delays
        // this snapshot -- wait (bounded) for the detached reader to drain the
        // pipe, then decode. Bounded so a descendant holding stderr can't stall.
        let deadline = Instant::now() + STDERR_DRAIN_WAIT;
        while !stderr_done.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        stderr_tail
            .lock()
            .map(|t| String::from_utf8_lossy(&t).trim().to_string())
            .unwrap_or_default()
    } else {
        folded.final_text
    };

    let summary = Summary {
        final_text,
        session_id: folded.session_id,
        model,
        is_error: folded.is_error,
        num_turns: folded.num_turns.max(1),
        total_cost_usd: 0.0,
        duration_api_ms: 0,
        usage: folded.usage,
        jsonl_replay: replay,
    };

    let mut streamed = false;
    if streaming
        && let Some(w) = stream_out.as_mut()
    {
        crate::emit::emit_result_envelope(*w, &summary, duration_ms).map_err(DriverError::Io)?;
        let _ = w.flush();
        streamed = true;
    }

    Ok(RunOutcome {
        summary,
        duration_ms,
        streamed,
        invalid_model: folded.invalid_model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_stream_into_answer_usage_session() {
        let lines = [
            r#"{"type":"thread.started","thread_id":"abc-123"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hello"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":12,"reasoning_output_tokens":5}}"#,
        ];
        let mut f = Folded::default();
        for l in lines {
            fold_event(&mut f, l);
        }
        assert_eq!(f.final_text, "hello");
        assert_eq!(f.session_id, "abc-123");
        assert_eq!(f.num_turns, 1);
        assert_eq!(f.usage.input_tokens, 100);
        assert_eq!(f.usage.output_tokens, 12);
        assert_eq!(f.usage.cache_read_input_tokens, 40);
        assert_eq!(f.usage.cache_creation_input_tokens, 0);
        assert!(!f.is_error);
    }

    #[test]
    fn last_agent_message_wins() {
        let mut f = Folded::default();
        fold_event(&mut f, r#"{"type":"item.completed","item":{"type":"agent_message","text":"first"}}"#);
        fold_event(&mut f, r#"{"type":"item.completed","item":{"type":"agent_message","text":"second"}}"#);
        assert_eq!(f.final_text, "second");
    }

    #[test]
    fn non_message_items_ignored() {
        let mut f = Folded::default();
        fold_event(&mut f, r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#);
        assert_eq!(f.final_text, "");
    }

    #[test]
    fn failure_event_marks_error() {
        let mut f = Folded::default();
        fold_event(&mut f, r#"{"type":"turn.failed","error":{"message":"boom"}}"#);
        assert!(f.is_error);
        assert!(!f.invalid_model);
        assert_eq!(f.error_message, "boom");
    }

    #[test]
    fn model_rejection_flags_invalid_model() {
        let mut f = Folded::default();
        fold_event(
            &mut f,
            r#"{"type":"error","message":"The 'bogus' model is not supported when using Codex with a ChatGPT account."}"#,
        );
        assert!(f.is_error);
        assert!(f.invalid_model);
    }

    #[test]
    fn detects_model_error_phrasings() {
        assert!(looks_like_model_error("The 'x' model is not supported"));
        assert!(looks_like_model_error("unknown model: y"));
        assert!(looks_like_model_error("that model does not exist"));
        assert!(!looks_like_model_error("rate limit exceeded"));
        assert!(!looks_like_model_error("the network is unreachable"));
    }

    #[test]
    fn malformed_and_unknown_lines_ignored() {
        let mut f = Folded::default();
        fold_event(&mut f, "not json");
        fold_event(&mut f, r#"{"type":"some.future.event","x":1}"#);
        assert_eq!(f, Folded::default());
    }

    #[test]
    fn model_from_rollout_takes_last_turn_context() {
        let contents = concat!(
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"x","model_provider":"openai"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/x"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5-codex"}}"#,
        );
        assert_eq!(model_from_rollout(contents).as_deref(), Some("gpt-5.5-codex"));
    }

    #[test]
    fn model_from_rollout_none_when_absent() {
        let contents = r#"{"type":"session_meta","payload":{"id":"x"}}"#;
        assert_eq!(model_from_rollout(contents), None);
    }

    #[test]
    fn build_argv_maps_flags() {
        let opts = Options {
            model: Some("gpt-5.5".into()),
            cwd: Some("/work".into()),
            skip_permissions: true,
            ..Options::default()
        };
        let v = build_argv(&opts);
        assert_eq!(v[0], "exec");
        assert!(v.contains(&"--json".to_string()));
        assert!(v.windows(2).any(|w| w == ["--model", "gpt-5.5"]));
        assert!(v.windows(2).any(|w| w == ["--cd", "/work"]));
        assert!(v.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    }
}
