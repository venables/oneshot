//! anyagent: one non-interactive interface in front of any coding-agent CLI.
//!
//! A thin adapter, not an orchestrator: it normalizes how you *invoke* and
//! *observe* a one-shot agent run across harnesses while preserving each
//! agent's native behavior. stdout carries only the answer; the authoritative
//! run metadata (`--meta-file`) reports the truth about what model ran and what
//! was actually enforced -- the two things every harness is otherwise vague
//! about. Commands: `run` (default; bare prompt is sugar), `list`,
//! `capabilities`. Adapters live in `src/adapters/` (claude via PTY + Stop
//! hook; codex via `codex exec`).

mod adapters;
mod args;
mod command;
mod dec;
mod emit;
mod harness;
mod hook;
mod meta;
mod policy;
mod pty;
mod signals;
mod stream;
mod transcript;

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use args::{Options, OutputFormat};
use command::Command;
use meta::{ExitStatus, Metadata};

/// Write the authoritative metadata envelope to `--meta-file` when requested.
/// Best-effort: a write failure warns on stderr but does not change the run's
/// outcome (the answer on stdout is what matters).
fn write_meta_file(opts: &Options, metadata: &Metadata) {
    let Some(path) = &opts.meta_file else { return };
    let json = metadata.to_json().to_string();
    if let Err(e) = std::fs::write(path, format!("{json}\n")) {
        eprintln!("anyagent: failed writing --meta-file {path}: {e}");
    }
}

fn main() -> ExitCode {
    signals::install();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let command = match command::parse(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("anyagent: {e}");
            return ExitCode::from(2);
        }
    };

    match command {
        Command::Run(opts) => run(*opts),
        Command::ListHarnesses => render(command::list_harnesses),
        Command::ListModels { harness } => render(|w| command::list_models(w, harness)),
        Command::Capabilities { harness } => render(|w| command::capabilities(w, harness)),
    }
}

/// Run a discovery command (`list`/`capabilities`) to stdout.
fn render(f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> ExitCode {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match f(&mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("anyagent: write failed: {e}");
            ExitCode::from(ExitStatus::Internal.code())
        }
    }
}

fn run(mut opts: Options) -> ExitCode {
    // Resolve the adapter that drives the selected harness. Reserved
    // harness names (codex, gemini, ...) have no adapter yet, so they fail
    // fast rather than silently behaving like claude.
    let adapter = match adapters::for_harness(&opts.harness) {
        Some(a) => a,
        None => {
            eprintln!(
                "anyagent: the '{}' harness is recognised but not implemented yet \
                 (today: claude, or a path to a claude-compatible binary). \
                 Recognised names: {}.",
                opts.harness.name(),
                harness::KNOWN_NAMES.join(", ")
            );
            write_meta_file(
                &opts,
                &Metadata::build(&opts, None, 0, ExitStatus::HarnessNotFound, None),
            );
            return ExitCode::from(ExitStatus::HarnessNotFound.code());
        }
    };

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

    // The enforcement class achieved for the requested perms tier (if any),
    // reported in metadata and used for the --require-enforcement preflight.
    let enforcement = opts.perms.map(|p| adapter.perms_enforcement(p));

    // Fail fast, before spawning, if the harness can't meet a demanded
    // enforcement class. This is what turns "the prompt is a firewall, not a
    // sandbox" into an actual guarantee.
    if let Err(msg) = adapters::check_enforcement(adapter.as_ref(), &opts) {
        eprintln!("anyagent: {msg}");
        write_meta_file(
            &opts,
            &Metadata::build(&opts, None, 0, ExitStatus::EnforcementUnsupported, enforcement),
        );
        return ExitCode::from(ExitStatus::EnforcementUnsupported.code());
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

    match adapter.run(&opts, stream_arg) {
        Ok(outcome) => {
            let status = if outcome.invalid_model {
                ExitStatus::InvalidModel
            } else if outcome.summary.is_error {
                ExitStatus::AgentError
            } else {
                ExitStatus::Ok
            };
            let metadata = Metadata::build(
                &opts,
                Some(&outcome.summary),
                outcome.duration_ms,
                status,
                enforcement,
            );
            write_meta_file(&opts, &metadata);

            if !outcome.streamed {
                let res = match opts.output_format {
                    OutputFormat::Text => emit::emit_text(&mut out, &outcome.summary),
                    OutputFormat::Json => {
                        emit::emit_answer_json(&mut out, &outcome.summary, &metadata)
                    }
                    OutputFormat::StreamJson => {
                        // Reached only if no stream writer was available; fall
                        // back to a buffered replay + result envelope.
                        out.write_all(outcome.summary.jsonl_replay.as_bytes()).and_then(|_| {
                            emit::emit_result_envelope(&mut out, &outcome.summary, outcome.duration_ms)
                        })
                    }
                };
                if let Err(e) = res {
                    eprintln!("anyagent: write failed: {e}");
                    return ExitCode::from(ExitStatus::Internal.code());
                }
            }
            ExitCode::from(status.code())
        }
        Err(e) => {
            eprintln!("anyagent: {e}");
            let status = e.status();
            write_meta_file(&opts, &Metadata::build(&opts, None, 0, status, enforcement));
            ExitCode::from(status.code())
        }
    }
}
