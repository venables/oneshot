//! anyagent: a drop-in replacement for `claude -p` that drives the interactive
//! `claude` TUI under a PTY and captures the final assistant message via a
//! Stop hook. Output on stdout matches `claude -p` for the same prompt.

mod args;
mod dec;
mod driver;
mod emit;
mod harness;
mod hook;
mod pty;
mod signals;
mod stream;
mod transcript;

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use args::OutputFormat;

fn main() -> ExitCode {
    signals::install();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = match args::parse(&raw) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("anyagent: {e}");
            return ExitCode::from(2);
        }
    };

    // Only the Claude protocol is implemented today. Reserved harness names
    // (codex, gemini, ...) fail fast rather than silently behaving like claude.
    if !opts.harness.is_supported() {
        eprintln!(
            "anyagent: the '{}' harness is recognised but not implemented yet \
             (today: claude, or a path to a claude-compatible binary). \
             Recognised names: {}.",
            opts.harness.name(),
            harness::KNOWN_NAMES.join(", ")
        );
        return ExitCode::from(2);
    }

    // No positional prompt: read it from stdin (so multiline prompts and pipes
    // work without shell escaping).
    if opts.prompt.is_empty() {
        if std::io::stdin().is_terminal() {
            eprintln!("anyagent: no prompt given (pass a prompt argument or pipe one on stdin)");
            return ExitCode::from(2);
        }
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            eprintln!("anyagent: failed reading stdin: {e}");
            return ExitCode::from(2);
        }
        opts.prompt = s.trim_end_matches('\n').to_string();
    }

    if opts.prompt.is_empty() {
        eprintln!("anyagent: empty prompt");
        return ExitCode::from(2);
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // For stream-json, hand the driver our stdout so it can emit transcript
    // lines live as claude flushes them.
    let stream_arg: Option<&mut dyn Write> = if opts.output_format == OutputFormat::StreamJson {
        Some(&mut out)
    } else {
        None
    };

    match driver::run(&opts, stream_arg) {
        Ok(outcome) => {
            if !outcome.streamed {
                let res = match opts.output_format {
                    OutputFormat::Text => emit::emit_text(&mut out, &outcome.summary),
                    OutputFormat::Json => {
                        emit::emit_json(&mut out, &outcome.summary, outcome.duration_ms)
                    }
                    OutputFormat::StreamJson => {
                        // Reached only if no stream writer was available; fall
                        // back to a buffered replay + result envelope.
                        out.write_all(outcome.summary.jsonl_replay.as_bytes()).and_then(|_| {
                            emit::emit_json(&mut out, &outcome.summary, outcome.duration_ms)
                        })
                    }
                };
                if let Err(e) = res {
                    eprintln!("anyagent: write failed: {e}");
                    return ExitCode::from(2);
                }
            }
            if outcome.summary.is_error {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("anyagent: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
