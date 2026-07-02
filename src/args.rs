//! Argument parsing for the `claude -p`-compatible surface. Recognised flags
//! are mapped onto the child `claude` invocation; anything we don't recognise
//! is forwarded verbatim so the wrapper stays useful as Claude Code evolves.
//!
//! `-p` / `--print` and a user-supplied `--settings` are rejected: we emulate
//! print mode by driving interactive mode, and we inject our own `--settings`
//! to register the Stop hook.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub prompt: String,
    pub output_format: OutputFormat,
    pub model: Option<String>,
    pub skip_permissions: bool,
    pub cwd: Option<String>,
    pub timeout_ms: u64,
    pub debug: bool,
    pub cols: u16,
    pub rows: u16,
    /// Flags forwarded verbatim to the child `claude`.
    pub extra_args: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            output_format: OutputFormat::Text,
            model: None,
            skip_permissions: false,
            cwd: None,
            timeout_ms: 300_000,
            debug: false,
            cols: 120,
            rows: 40,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArgError {
    PrintModeRejected,
    SettingsRejected,
    MissingValue(String),
    BadOutputFormat(String),
    BadNumber(String),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrintModeRejected => write!(
                f,
                "-p/--print is rejected: claude-p emulates print mode by driving interactive mode"
            ),
            Self::SettingsRejected => write!(
                f,
                "--settings is rejected: claude-p injects its own settings to register the Stop hook"
            ),
            Self::MissingValue(flag) => write!(f, "flag {flag} requires a value"),
            Self::BadOutputFormat(v) => {
                write!(f, "invalid --output-format {v:?} (text|json|stream-json)")
            }
            Self::BadNumber(v) => write!(f, "invalid number: {v:?}"),
        }
    }
}

/// Parse argv (excluding argv[0]). Positional tokens become the prompt.
pub fn parse(args: &[String]) -> Result<Options, ArgError> {
    let mut opts = Options::default();
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut end_of_options = false;

    while i < args.len() {
        let a = &args[i];
        if end_of_options {
            prompt_parts.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => end_of_options = true,
            "-p" | "--print" => return Err(ArgError::PrintModeRejected),
            "--settings" => return Err(ArgError::SettingsRejected),
            "--dangerously-skip-permissions" => opts.skip_permissions = true,
            "--debug" | "-d" => opts.debug = true,
            "--output-format" => {
                opts.output_format = parse_output_format(take_value(args, &mut i, a)?)?;
            }
            "--model" => opts.model = Some(take_value(args, &mut i, a)?.to_string()),
            "--cwd" => opts.cwd = Some(take_value(args, &mut i, a)?.to_string()),
            "--timeout" => {
                let secs: u64 = take_value(args, &mut i, a)?
                    .parse()
                    .map_err(|_| ArgError::BadNumber(args[i].clone()))?;
                opts.timeout_ms = secs.saturating_mul(1000);
            }
            "--cols" => {
                opts.cols = take_value(args, &mut i, a)?
                    .parse()
                    .map_err(|_| ArgError::BadNumber(args[i].clone()))?;
            }
            "--rows" => {
                opts.rows = take_value(args, &mut i, a)?
                    .parse()
                    .map_err(|_| ArgError::BadNumber(args[i].clone()))?;
            }
            other if other.starts_with('-') => {
                // Unknown flag: forward verbatim. We can't know its arity, so
                // we forward only the flag token; flags-with-values should be
                // passed as `--flag=value` to survive, or after `--` for the
                // prompt. (Spike limitation; documented.)
                opts.extra_args.push(other.to_string());
            }
            _ => prompt_parts.push(a.clone()),
        }
        i += 1;
    }

    opts.prompt = prompt_parts.join(" ");
    Ok(opts)
}

fn parse_output_format(v: &str) -> Result<OutputFormat, ArgError> {
    match v {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        "stream-json" => Ok(OutputFormat::StreamJson),
        other => Err(ArgError::BadOutputFormat(other.to_string())),
    }
}

fn take_value<'a>(
    args: &'a [String],
    i: &mut usize,
    flag: &str,
) -> Result<&'a str, ArgError> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| ArgError::MissingValue(flag.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn positional_prompt() {
        let o = parse(&v(&["hello", "world"])).unwrap();
        assert_eq!(o.prompt, "hello world");
        assert_eq!(o.output_format, OutputFormat::Text);
    }

    #[test]
    fn output_format_json() {
        let o = parse(&v(&["--output-format", "json", "hi"])).unwrap();
        assert_eq!(o.output_format, OutputFormat::Json);
        assert_eq!(o.prompt, "hi");
    }

    #[test]
    fn skip_permissions_and_model() {
        let o = parse(&v(&["--dangerously-skip-permissions", "--model", "opus", "hi"])).unwrap();
        assert!(o.skip_permissions);
        assert_eq!(o.model.as_deref(), Some("opus"));
    }

    #[test]
    fn print_mode_rejected() {
        assert_eq!(parse(&v(&["-p", "hi"])), Err(ArgError::PrintModeRejected));
    }

    #[test]
    fn settings_rejected() {
        assert_eq!(
            parse(&v(&["--settings", "{}", "hi"])),
            Err(ArgError::SettingsRejected)
        );
    }

    #[test]
    fn timeout_seconds_to_ms() {
        let o = parse(&v(&["--timeout", "30", "hi"])).unwrap();
        assert_eq!(o.timeout_ms, 30_000);
    }

    #[test]
    fn end_of_options_routes_to_prompt() {
        let o = parse(&v(&["--", "--model", "literal"])).unwrap();
        assert_eq!(o.prompt, "--model literal");
    }

    #[test]
    fn unknown_flag_forwarded() {
        let o = parse(&v(&["--verbose", "hi"])).unwrap();
        assert!(o.extra_args.contains(&"--verbose".to_string()));
        assert_eq!(o.prompt, "hi");
    }

    #[test]
    fn bad_output_format() {
        assert!(matches!(
            parse(&v(&["--output-format", "yaml", "hi"])),
            Err(ArgError::BadOutputFormat(_))
        ));
    }
}
