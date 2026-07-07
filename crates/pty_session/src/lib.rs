//! Interactive process sessions for agent tools.
//!
//! `command_executor` covers the classic one-shot case: run a command,
//! block, return all output. This crate covers everything that model
//! doesn't fit: processes that outlive a single tool call, interactive
//! programs on a real PTY (ssh, sudo prompts, REPLs), and background jobs
//! the agent polls later.
//!
//! Building blocks:
//! - [`PtySession`] — one spawned process (PTY or plain pipes), with
//!   incremental output windows, stdin writes, interrupt and terminate.
//! - [`PtySessionManager`] — id-keyed registry with an LRU cap; one per
//!   agent session, so sessions survive across tool calls but die with
//!   their agent session.
//! - [`HeadTailBuffer`] — byte-capped buffer keeping head and tail of
//!   unbounded output.
//!
//! The crate is UI-free and sandbox-agnostic: callers pass a full argv, so
//! a sandboxed invocation (e.g. seatbelt) is just a different argv.

mod buffer;
mod manager;
mod session;

pub use buffer::{BufferedBytes, BufferedOutput, HeadTailBuffer};
pub use manager::{DEFAULT_MAX_SESSIONS, PtySessionInfo, PtySessionManager};
pub use session::{
    CTRL_C, CollectedOutput, DEFAULT_MAX_BUFFER_BYTES, PtySession, PtySessionStatus,
    PtySpawnConfig, TerminalOutputSink, sanitize_terminal_output,
};
