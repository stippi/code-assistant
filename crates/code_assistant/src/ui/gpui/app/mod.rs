//! Application-level orchestration for the GPUI interface.
//!
//! This module contains the event processing loop, backend communication,
//! the `UserInterface` trait implementation, and draft persistence — all the
//! "glue" that connects the UI components to the agent/session system.

pub(super) mod backend;
pub(super) mod drafts;
pub(super) mod event_loop;
pub(super) mod user_interface_impl;
