//! Sub-agents' transcripts as sessions of their own, inside their parent's
//! folder (see [`crate::persistence::layout::sub_agent_session_id`]): any
//! frontend opens one like a session, read-only.

use crate::agent::persistence::AgentStatePersistence;
use crate::agent::sub_agent::{SubAgentTranscript, SubAgentTranscripts};
use crate::persistence::{ChatSession, FileSessionPersistence, SessionModelConfig};
use crate::session::event_stream::EventStream;
use crate::session::{SessionCheckpoint, SessionConfig};
use anyhow::Result;
use std::sync::Arc;

/// Creates the transcripts of one session's sub-agents.
pub struct SessionTranscripts {
    pub parent_session_id: String,
    pub persistence: FileSessionPersistence,
    pub events: EventStream,
    pub tool_registry: Arc<crate::tools::core::ToolRegistry>,
    pub config: SessionConfig,
    pub model: Option<SessionModelConfig>,
}

impl SubAgentTranscripts for SessionTranscripts {
    fn create(&self, agent_id: u64, title: &str) -> Result<SubAgentTranscript> {
        let session_id =
            crate::persistence::layout::sub_agent_session_id(&self.parent_session_id, agent_id);
        let session = ChatSession::new_empty(
            session_id.clone(),
            format!("Sub-agent #{agent_id}: {title}"),
            self.config.clone(),
            self.model.clone(),
        );
        let mut persistence = self.persistence.clone();
        persistence.create_chat_session(&session)?;
        // Publishes into the transcript the way a session's own agent does;
        // the instance only lends its buffers.
        let ui =
            crate::session::instance::SessionInstance::new(session, self.tool_registry.clone())
                .create_publisher(self.events.clone(), None);
        Ok(SubAgentTranscript {
            session_id,
            persistence: Box::new(TranscriptPersistence { persistence }),
            ui,
        })
    }
}

/// Commits a sub-agent's checkpoints straight into its transcript.
struct TranscriptPersistence {
    persistence: FileSessionPersistence,
}

impl AgentStatePersistence for TranscriptPersistence {
    fn commit_checkpoint(&mut self, checkpoint: SessionCheckpoint<'_>) -> Result<()> {
        self.persistence.commit_checkpoint(&checkpoint).map(|_| ())
    }
}
