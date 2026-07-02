//! Spawn `claude` under a real PTY and expose the master read/write halves
//! plus the child handle. We exec the argv directly on the PTY slave -- no
//! `sh -c` indirection, so there is no shell-quoting layer to get wrong.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use anyhow::Result;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub struct Spawned {
    pub child: Box<dyn Child + Send + Sync>,
    /// Held to keep the master fd open for the lifetime of the session.
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

pub struct SpawnConfig<'a> {
    pub argv: &'a [String],
    pub cwd: Option<&'a str>,
    pub extra_env: &'a BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

pub fn spawn(cfg: SpawnConfig) -> Result<Spawned> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: cfg.rows,
        cols: cfg.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&cfg.argv[0]);
    for arg in &cfg.argv[1..] {
        cmd.arg(arg);
    }

    // Deterministic environment: start from the parent's, then apply our
    // overrides. Forcing TERM keeps Ink's terminal probing on the path our
    // DEC responder handles.
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    for (k, v) in cfg.extra_env {
        cmd.env(k, v);
    }

    if let Some(dir) = cfg.cwd {
        cmd.cwd(dir);
    }

    let child = pair.slave.spawn_command(cmd)?;
    // Drop the slave so the master sees EOF when the child exits.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok(Spawned {
        child,
        master: pair.master,
        reader,
        writer,
    })
}
