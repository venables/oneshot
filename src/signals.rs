//! Minimal SIGINT/SIGTERM handling. We install handlers that flip a global
//! flag rather than letting the default disposition kill us, so the driver's
//! main loop can notice the interrupt, tear the child down cleanly (kill its
//! process group, remove the temp dir), and exit 130. Without this a Ctrl-C
//! orphans the child `claude` process.

use std::sync::atomic::{AtomicBool, Ordering};

use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle(_: i32) {
    // Async-signal-safe: a single atomic store.
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install handlers for SIGINT and SIGTERM. Idempotent enough for our use
/// (called once at startup). We deliberately omit `SA_RESTART` so blocked
/// syscalls return `EINTR` and the loop gets a chance to observe the flag.
pub fn install() {
    let action = SigAction::new(SigHandler::Handler(handle), SaFlags::empty(), SigSet::empty());
    // SAFETY: the handler only performs an atomic store, which is
    // async-signal-safe.
    unsafe {
        let _ = signal::sigaction(Signal::SIGINT, &action);
        let _ = signal::sigaction(Signal::SIGTERM, &action);
    }
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
