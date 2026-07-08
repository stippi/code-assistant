//! Per-session cancellation flags for in-flight *blocking* (foreground)
//! `execute_command` invocations, keyed by tool_id.
//!
//! Background (session-mode) commands are interrupted directly through the
//! [`PtySessionManager`](pty_session::PtySessionManager), which owns their
//! `PtySession`. A classic blocking command's PTY, by contrast, lives inside
//! the command executor for the duration of the call and can only be reached
//! via a shared flag the executor's streaming callback polls — that is what
//! this registry provides. The UI's terminal-card stop button sets the flag;
//! the executor sees it on its next poll window and interrupts the process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Cancel flags for running blocking commands, keyed by tool_id.
#[derive(Default)]
pub struct TerminalInterrupts {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TerminalInterrupts {
    /// Register a fresh cancel flag for a starting command and return it. The
    /// command's streaming callback polls the flag; the UI sets it via
    /// [`request`](Self::request).
    pub fn register(&self, tool_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.flags
            .lock()
            .unwrap()
            .insert(tool_id.to_string(), flag.clone());
        flag
    }

    /// Stop tracking a finished command.
    pub fn unregister(&self, tool_id: &str) {
        self.flags.lock().unwrap().remove(tool_id);
    }

    /// Request cancellation of the blocking command owning `tool_id`. Returns
    /// `true` if a matching in-flight command was found.
    pub fn request(&self, tool_id: &str) -> bool {
        if let Some(flag) = self.flags.lock().unwrap().get(tool_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_sets_the_flag_for_a_registered_command() {
        let interrupts = TerminalInterrupts::default();
        let flag = interrupts.register("tool-1");
        assert!(!flag.load(Ordering::Relaxed));

        // Unknown tool_id: no match.
        assert!(!interrupts.request("other"));
        assert!(!flag.load(Ordering::Relaxed));

        // Registered tool_id: flag is set.
        assert!(interrupts.request("tool-1"));
        assert!(flag.load(Ordering::Relaxed));

        // After unregister, there is nothing left to signal.
        interrupts.unregister("tool-1");
        assert!(!interrupts.request("tool-1"));
    }
}
