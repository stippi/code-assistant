//! Read-only query engine over the persisted session store.
//!
//! This is a *deep module*: the surface is two operations —
//!
//! * [`search_sessions`] — find sessions across the store by cheap metadata
//!   filters (project, name, time) and/or by the tool calls they contain
//!   (e.g. "which sessions ran `write_file`/`edit` on a path under
//!   `docs/`"), returning compact matches.
//! * [`get_session_content`] — pull a *filtered projection* of a single
//!   session (user messages, assistant replies, thinking, tool calls, tool
//!   results — any combination, optionally narrowed to certain tools, a
//!   message range, and truncated), so callers can read only what they need
//!   without loading a whole transcript.
//!
//! Everything underneath — walking the conversation tree, flattening it into
//! typed content items, correlating tool results back to their calls, and
//! evaluating string matchers — is hidden behind those two calls.
//!
//! ## Seams
//!
//! The engine never touches the filesystem directly. It reads through the
//! narrow [`SessionSource`] trait, implemented for
//! [`FileSessionPersistence`](crate::persistence::FileSessionPersistence) in
//! production and by an in-memory source in tests. The internal layers are
//! independently testable:
//!
//! * [`matcher`] — declarative [`StringMatch`] compiled into a validated matcher.
//! * [`extract`] — pure `ChatSession` → `Vec<ExtractedItem>` flattening.
//! * [`search`] / [`projection`] — orchestration over a [`SessionSource`].

use anyhow::Result;

use crate::persistence::{ChatMetadata, ChatSession, FileSessionPersistence};

pub mod extract;
pub mod matcher;
pub mod projection;
pub mod search;

pub use extract::{ContentKind, ExtractedItem, Role, extract_items};
pub use matcher::StringMatch;
pub use projection::{
    ContentItem, ContentPart, ContentProjection, MessageRange, SessionContent, get_session_content,
};
pub use search::{
    SessionMatch, SessionSearchQuery, ToolCallFilter, ToolCallMatch, search_sessions,
};

/// Read-only access to the session store the query engine operates on.
///
/// The two methods mirror what a metadata-indexed store offers cheaply: a
/// listing of lightweight [`ChatMetadata`] (used for the coarse, no-load
/// pre-filter) and a full [`ChatSession`] load on demand (only for sessions
/// that survive the pre-filter or are read in full).
///
/// `Send + Sync` so a source can be shared (`Arc<dyn SessionSource>`) across
/// the tool services that travel type-erased into a tool invocation.
pub trait SessionSource: Send + Sync {
    /// The metadata index of all sessions, newest first (as the persistence
    /// layer returns it).
    fn list_metadata(&self) -> Result<Vec<ChatMetadata>>;

    /// Load a full session by id, or `None` if it does not exist.
    fn load(&self, session_id: &str) -> Result<Option<ChatSession>>;
}

impl SessionSource for FileSessionPersistence {
    fn list_metadata(&self) -> Result<Vec<ChatMetadata>> {
        self.list_chat_sessions()
    }

    fn load(&self, session_id: &str) -> Result<Option<ChatSession>> {
        self.load_chat_session(session_id)
    }
}

#[cfg(test)]
pub(crate) mod test_source {
    //! An in-memory [`SessionSource`] for the engine's own tests.

    use super::*;
    use std::collections::HashMap;

    /// In-memory session store. Metadata is derived from the sessions so
    /// tests only have to supply [`ChatSession`] values.
    #[derive(Default)]
    pub struct InMemorySource {
        sessions: HashMap<String, ChatSession>,
    }

    impl InMemorySource {
        pub fn new() -> Self {
            Self::default()
        }

        /// Add or replace a session.
        pub fn with_session(mut self, session: ChatSession) -> Self {
            self.sessions.insert(session.id.clone(), session);
            self
        }
    }

    impl SessionSource for InMemorySource {
        fn list_metadata(&self) -> Result<Vec<ChatMetadata>> {
            let mut metadata: Vec<ChatMetadata> = self
                .sessions
                .values()
                .map(|session| {
                    let mut session = session.clone();
                    // Normalize legacy fields the same way persistence does.
                    session.ensure_config().ok();
                    ChatMetadata {
                        id: session.id.clone(),
                        name: session.name.clone(),
                        created_at: session.created_at,
                        updated_at: session.updated_at,
                        message_count: session.message_count(),
                        total_usage: llm::Usage::zero(),
                        last_usage: llm::Usage::zero(),
                        tokens_limit: None,
                        tool_syntax: session.tool_syntax(),
                        initial_project: session.initial_project().to_string(),
                        plan_collapsed: session.plan_collapsed,
                        is_resumable: session.is_resumable(),
                    }
                })
                .collect();
            // Persistence returns newest first.
            metadata.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
            Ok(metadata)
        }

        fn load(&self, session_id: &str) -> Result<Option<ChatSession>> {
            Ok(self.sessions.get(session_id).cloned().map(|mut session| {
                session.ensure_config().ok();
                session
            }))
        }
    }
}
