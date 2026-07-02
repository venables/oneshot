//! Process-group teardown for the native subprocess adapters.
//!
//! `claude -p` and `codex exec` spawn their own tool subprocesses. A bare
//! `Child::kill` signals only the top-level process, orphaning that whole tree
//! on interrupt/timeout. So we make each child a session/group leader
//! (`setsid` in a `pre_exec` hook) and, on teardown, signal the entire group.

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

/// Make the spawned child lead its own process group, so its tool subprocesses
/// can be torn down as a unit. Must be called before `spawn`.
pub fn lead_process_group(cmd: &mut Command) {
    // SAFETY: `setsid` is async-signal-safe and touches only the forked child
    // between fork and exec; it makes the child a new session + group leader.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from)
        });
    }
}

/// Terminate the child's process group: SIGTERM, a short grace period, then
/// SIGKILL. After `lead_process_group` the child's PID is also its PGID, so the
/// PID doubles as the group id. Best-effort -- a dead group just yields ESRCH.
pub fn terminate_group(pid: u32) {
    let pgid = Pid::from_raw(pid as i32);
    let _ = signal::killpg(pgid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(200));
    let _ = signal::killpg(pgid, Signal::SIGKILL);
}
