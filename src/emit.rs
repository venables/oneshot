//! Output formatters. `text` prints the final assistant message; `json`
//! prints a single `result` envelope shaped like `claude -p --output-format
//! json`. `stream-json` reuses the json envelope as the trailing line after
//! the live transcript replay the driver emits.

use std::io::Write;

use crate::transcript::Summary;

pub fn emit_text(w: &mut dyn Write, summary: &Summary) -> std::io::Result<()> {
    writeln!(w, "{}", summary.final_text)
}

pub fn emit_json(w: &mut dyn Write, summary: &Summary, duration_ms: u64) -> std::io::Result<()> {
    let envelope = serde_json::json!({
        "type": "result",
        "subtype": if summary.is_error { "error" } else { "success" },
        "session_id": summary.session_id,
        "result": summary.final_text,
        "is_error": summary.is_error,
        "duration_ms": duration_ms,
        "duration_api_ms": summary.duration_api_ms,
        "num_turns": summary.num_turns,
        "total_cost_usd": summary.total_cost_usd,
        "usage": {
            "input_tokens": summary.usage.input_tokens,
            "output_tokens": summary.usage.output_tokens,
            "cache_read_input_tokens": summary.usage.cache_read_input_tokens,
            "cache_creation_input_tokens": summary.usage.cache_creation_input_tokens,
        },
        "permission_denials": [],
    });
    writeln!(w, "{envelope}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Usage;

    fn summary() -> Summary {
        Summary {
            final_text: "OK".into(),
            session_id: "sid".into(),
            is_error: false,
            num_turns: 1,
            total_cost_usd: 0.0,
            duration_api_ms: 0,
            usage: Usage {
                input_tokens: 6,
                output_tokens: 6,
                ..Default::default()
            },
            jsonl_replay: String::new(),
        }
    }

    #[test]
    fn text_is_message_plus_newline() {
        let mut buf = Vec::new();
        emit_text(&mut buf, &summary()).unwrap();
        assert_eq!(buf, b"OK\n");
    }

    #[test]
    fn json_envelope_shape() {
        let mut buf = Vec::new();
        emit_json(&mut buf, &summary(), 2911).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "success");
        assert_eq!(v["result"], "OK");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["duration_ms"], 2911);
        assert_eq!(v["usage"]["input_tokens"], 6);
        assert!(v["permission_denials"].is_array());
    }
}
