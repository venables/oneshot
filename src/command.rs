//! Top-level command surface: `run`, `list`, `capabilities`, plus the
//! bare-prompt sugar that forwards to `run` with defaults.
//!
//! `run`/`list`/`capabilities` are recognised only as the *first* argument
//! (like git/docker); anything else is treated as a prompt for `run`, so
//! `anyagent "summarize this"` still works. A prompt that literally starts with
//! one of those words can be forced with `anyagent run -- "run the tests"`.

use std::io::Write;

use crate::adapters::{self, Adapter};
use crate::args::{self, ArgError, Options};
use crate::harness::{Harness, KNOWN_NAMES};
use crate::policy::{Network, Perms};

pub enum Command {
    Run(Box<Options>),
    ListHarnesses,
    ListModels { harness: Option<Harness> },
    Capabilities { harness: Option<Harness> },
}

/// Parse argv (excluding argv[0]) into a command.
pub fn parse(raw: &[String]) -> Result<Command, ArgError> {
    match raw.first().map(String::as_str) {
        Some("run") => Ok(Command::Run(Box::new(args::parse(&raw[1..])?))),
        Some("list") => parse_list(&raw[1..]),
        Some("capabilities") | Some("caps") => Ok(Command::Capabilities {
            harness: harness_flag(&raw[1..])?,
        }),
        // Bare prompt / flags: sugar for `run`.
        _ => Ok(Command::Run(Box::new(args::parse(raw)?))),
    }
}

fn parse_list(rest: &[String]) -> Result<Command, ArgError> {
    match rest.first().map(String::as_str) {
        Some("harnesses") => Ok(Command::ListHarnesses),
        Some("models") => Ok(Command::ListModels {
            harness: harness_flag(&rest[1..])?,
        }),
        _ => Err(ArgError::Usage(
            "list: expected 'harnesses' or 'models'".to_string(),
        )),
    }
}

/// Scan for a `-H`/`--harness <name>` (or `=name`) flag in `rest`.
fn harness_flag(rest: &[String]) -> Result<Option<Harness>, ArgError> {
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (a.as_str(), None),
        };
        if flag == "-H" || flag == "--harness" {
            let val = match inline {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    rest.get(i)
                        .cloned()
                        .ok_or_else(|| ArgError::MissingValue(flag.to_string()))?
                }
            };
            return Ok(Some(Harness::parse(&val)));
        }
        i += 1;
    }
    Ok(None)
}

const PERM_TIERS: [Perms; 3] = [Perms::ReadOnly, Perms::WorkspaceWrite, Perms::Full];

/// `list harnesses`: every recognised harness, whether it has an adapter, and
/// whether its binary is installed (with version).
pub fn list_harnesses(w: &mut dyn Write) -> std::io::Result<()> {
    for name in KNOWN_NAMES {
        let h = Harness::parse(name);
        let status = if adapters::for_harness(&h).is_some() {
            "implemented"
        } else {
            "reserved"
        };
        let version = h.probe_version();
        let install = match &version {
            Some(v) => format!("installed ({v})"),
            None => "not found".to_string(),
        };
        writeln!(w, "{name:<10} {status:<12} {install}")?;
    }
    Ok(())
}

/// `capabilities`: the honest per-harness enforcement map, so callers stop
/// hardcoding harness knowledge. Defaults to every implemented harness.
pub fn capabilities(w: &mut dyn Write, harness: Option<Harness>) -> std::io::Result<()> {
    let targets: Vec<Harness> = match harness {
        Some(h) => vec![h],
        None => KNOWN_NAMES.iter().map(|n| Harness::parse(n)).collect(),
    };
    let mut first = true;
    for h in targets {
        let Some(adapter) = adapters::for_harness(&h) else {
            continue;
        };
        if !first {
            writeln!(w)?;
        }
        first = false;
        render_capabilities(w, &h, adapter.as_ref())?;
    }
    Ok(())
}

/// `list models`: best-effort, honest about each harness's limits. Neither
/// codex nor claude exposes a clean model-enumeration command, so we probe what
/// we can (codex's configured default) and otherwise point at the aliases.
pub fn list_models(w: &mut dyn Write, harness: Option<Harness>) -> std::io::Result<()> {
    let targets: Vec<Harness> = match harness {
        Some(h) => vec![h],
        None => KNOWN_NAMES.iter().map(|n| Harness::parse(n)).collect(),
    };
    let mut first = true;
    for h in targets {
        if adapters::for_harness(&h).is_none() {
            continue;
        }
        if !first {
            writeln!(w)?;
        }
        first = false;
        writeln!(w, "harness: {}", h.name())?;
        match h {
            Harness::Codex => {
                match crate::adapters::codex::configured_model() {
                    Some(m) => writeln!(w, "  configured default: {m}")?,
                    None => writeln!(w, "  configured default: (unknown)")?,
                }
                writeln!(w, "  note: codex models are provider-defined; pass -m <model> (e.g. gpt-5.5)")?;
            }
            _ => {
                writeln!(w, "  aliases: opus, sonnet, haiku (or a full claude-* id)")?;
                writeln!(w, "  note: claude exposes no model-list API; model_resolved is read from the transcript")?;
            }
        }
    }
    Ok(())
}

fn render_capabilities(w: &mut dyn Write, h: &Harness, adapter: &dyn Adapter) -> std::io::Result<()> {
    writeln!(w, "harness: {}", h.name())?;
    writeln!(w, "perms:")?;
    for p in PERM_TIERS {
        writeln!(w, "  {:<16} {}", p.label(), adapter.perms_enforcement(p).label())?;
    }
    // Network control: does any sandboxed tier OS-enforce "no network"?
    let net = adapter.network_enforcement(Some(Perms::WorkspaceWrite), Network::None);
    let net_label = if net == crate::policy::Enforcement::OsSandbox {
        "yes (sandbox blocks network)"
    } else {
        "no"
    };
    writeln!(w, "network-control: {net_label}")?;
    writeln!(w, "output-modes: text, json, stream-json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_prompt_is_run() {
        let cmd = parse(&v(&["hello", "world"])).unwrap();
        match cmd {
            Command::Run(o) => assert_eq!(o.prompt, "hello world"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_subcommand_strips_keyword() {
        let cmd = parse(&v(&["run", "--model", "opus", "hi"])).unwrap();
        match cmd {
            Command::Run(o) => {
                assert_eq!(o.prompt, "hi");
                assert_eq!(o.model.as_deref(), Some("opus"));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn list_harnesses_and_models() {
        assert!(matches!(parse(&v(&["list", "harnesses"])).unwrap(), Command::ListHarnesses));
        match parse(&v(&["list", "models", "--harness", "codex"])).unwrap() {
            Command::ListModels { harness } => assert_eq!(harness, Some(Harness::Codex)),
            _ => panic!("expected ListModels"),
        }
    }

    #[test]
    fn list_without_target_errs() {
        assert!(matches!(parse(&v(&["list"])), Err(ArgError::Usage(_))));
        assert!(matches!(parse(&v(&["list", "bogus"])), Err(ArgError::Usage(_))));
    }

    #[test]
    fn capabilities_with_and_without_harness() {
        match parse(&v(&["capabilities"])).unwrap() {
            Command::Capabilities { harness } => assert_eq!(harness, None),
            _ => panic!("expected Capabilities"),
        }
        match parse(&v(&["capabilities", "--harness", "claude"])).unwrap() {
            Command::Capabilities { harness } => assert_eq!(harness, Some(Harness::Claude)),
            _ => panic!("expected Capabilities"),
        }
    }
}
