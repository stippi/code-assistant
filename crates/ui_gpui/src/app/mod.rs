//! Application-level orchestration for the GPUI interface.
//!
//! This module contains the event processing loop, the typed session
//! commands (send side), the broadcast-stream bridge (receive side), and
//! draft persistence — all the "glue" that connects the UI components to
//! the agent/session system.

pub(super) mod commands;
pub(super) mod drafts;
pub(super) mod event_bridge;
pub(super) mod event_loop;
