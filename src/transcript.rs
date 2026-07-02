//! Parser for a Claude Code session transcript (JSONL). Each line is one JSON
//! event. We extract the final assistant text, aggregated usage, the session
//! id, and error/result flags. We never write to the transcript.

use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub final_text: String,
    pub session_id: String,
    pub is_error: bool,
    pub num_turns: u32,
    pub total_cost_usd: f64,
    pub duration_api_ms: u64,
    pub usage: Usage,
    /// The transcript lines, verbatim, for stream-json replay.
    pub jsonl_replay: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    NoAssistantMessage,
}

/// Parse a transcript from raw JSONL bytes.
pub fn parse(bytes: &str) -> Result<Summary, ParseError> {
    let mut final_text = String::new();
    let mut session_id = String::new();
    let mut replay = String::new();
    let mut usage = Usage::default();
    let mut is_error = false;
    let mut num_turns: u32 = 0;
    let mut total_cost_usd = 0.0;
    let mut duration_api_ms: u64 = 0;
    let mut saw_assistant = false;

    for raw_line in bytes.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        replay.push_str(line);
        replay.push('\n');

        // Skip malformed lines rather than failing the whole parse.
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !obj.is_object() {
            continue;
        }

        // session id: `sessionId` (real transcripts) or `session_id` (legacy
        // stream-json). Accept both, prefer the first seen.
        if session_id.is_empty() {
            if let Some(s) = obj.get("sessionId").and_then(Value::as_str) {
                session_id = s.to_string();
            } else if let Some(s) = obj.get("session_id").and_then(Value::as_str) {
                session_id = s.to_string();
            }
        }

        let Some(ty) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };

        match ty {
            "assistant" => {
                saw_assistant = true;
                num_turns += 1;
                // We want the *last* assistant message; each new one resets.
                final_text.clear();
                if let Some(msg) = obj.get("message") {
                    if let Some(content) = msg.get("content").and_then(Value::as_array) {
                        for block in content {
                            if block.get("type").and_then(Value::as_str) == Some("text") {
                                if let Some(t) = block.get("text").and_then(Value::as_str) {
                                    final_text.push_str(t);
                                }
                            }
                        }
                    }
                    if let Some(u) = msg.get("usage") {
                        accumulate_usage(&mut usage, u);
                    }
                }
            }
            "result" => {
                // Final result event overrides text + flags when present.
                if let Some(r) = obj.get("result").and_then(Value::as_str) {
                    final_text.clear();
                    final_text.push_str(r);
                    saw_assistant = true;
                }
                if let Some(b) = obj.get("is_error").and_then(Value::as_bool) {
                    is_error = b;
                }
                if let Some(n) = obj.get("num_turns").and_then(Value::as_u64) {
                    num_turns = n as u32;
                }
                if let Some(c) = obj.get("total_cost_usd").and_then(Value::as_f64) {
                    total_cost_usd = c;
                }
                if let Some(d) = obj.get("duration_api_ms").and_then(Value::as_u64) {
                    duration_api_ms = d;
                }
            }
            _ => {}
        }
    }

    if !saw_assistant {
        return Err(ParseError::NoAssistantMessage);
    }

    Ok(Summary {
        final_text,
        session_id,
        is_error,
        num_turns,
        total_cost_usd,
        duration_api_ms,
        usage,
        jsonl_replay: replay,
    })
}

/// Parse from a file path.
pub fn parse_file(path: &std::path::Path) -> std::io::Result<Result<Summary, ParseError>> {
    let bytes = std::fs::read_to_string(path)?;
    Ok(parse(&bytes))
}

fn accumulate_usage(usage: &mut Usage, u: &Value) {
    let add = |dst: &mut u64, key: &str| {
        if let Some(v) = u.get(key).and_then(Value::as_u64) {
            *dst = dst.saturating_add(v);
        }
    };
    add(&mut usage.input_tokens, "input_tokens");
    add(&mut usage.output_tokens, "output_tokens");
    add(&mut usage.cache_read_input_tokens, "cache_read_input_tokens");
    add(
        &mut usage.cache_creation_input_tokens,
        "cache_creation_input_tokens",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_assistant_message() {
        let jsonl = r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"text","text":"Hello, world."}],"usage":{"input_tokens":10,"output_tokens":3}}}"#;
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "Hello, world.");
        assert_eq!(s.session_id, "abc");
        assert_eq!(s.num_turns, 1);
        assert_eq!(s.usage.input_tokens, 10);
        assert_eq!(s.usage.output_tokens, 3);
        assert!(!s.is_error);
    }

    #[test]
    fn last_assistant_wins_usage_accumulates() {
        let jsonl = concat!(
            r#"{"type":"system","subtype":"init","session_id":"xyz"}"#,
            "\n",
            r#"{"type":"assistant","session_id":"xyz","message":{"content":[{"type":"text","text":"first"}],"usage":{"input_tokens":5,"output_tokens":2}}}"#,
            "\n",
            r#"{"type":"assistant","session_id":"xyz","message":{"content":[{"type":"text","text":"final answer"}],"usage":{"input_tokens":7,"output_tokens":4}}}"#,
        );
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "final answer");
        assert_eq!(s.usage.input_tokens, 12);
        assert_eq!(s.usage.output_tokens, 6);
    }

    #[test]
    fn result_event_wins_for_text_and_flags() {
        let jsonl = concat!(
            r#"{"type":"assistant","session_id":"r","message":{"content":[{"type":"text","text":"draft"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","session_id":"r","result":"final","is_error":false,"num_turns":2,"total_cost_usd":0.0421,"duration_api_ms":9120}"#,
        );
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "final");
        assert!(!s.is_error);
        assert_eq!(s.num_turns, 2);
        assert!((s.total_cost_usd - 0.0421).abs() < 1e-9);
        assert_eq!(s.duration_api_ms, 9120);
    }

    #[test]
    fn error_result_event() {
        let jsonl = r#"{"type":"result","subtype":"error","session_id":"e","result":"failure detail","is_error":true}"#;
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "failure detail");
        assert!(s.is_error);
    }

    #[test]
    fn no_assistant_is_error() {
        let jsonl = r#"{"type":"system","subtype":"init","session_id":"x"}"#;
        assert_eq!(parse(jsonl), Err(ParseError::NoAssistantMessage));
    }

    #[test]
    fn skips_malformed_lines() {
        let jsonl = concat!(
            "not-json\n",
            r#"{"type":"assistant","session_id":"k","message":{"content":[{"type":"text","text":"alive"}]}}"#,
        );
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "alive");
    }

    #[test]
    fn multi_block_text_concatenates() {
        let jsonl = r#"{"type":"assistant","session_id":"c","message":{"content":[{"type":"text","text":"part1 "},{"type":"tool_use","name":"X"},{"type":"text","text":"part2"}]}}"#;
        let s = parse(jsonl).unwrap();
        assert_eq!(s.final_text, "part1 part2");
    }
}
