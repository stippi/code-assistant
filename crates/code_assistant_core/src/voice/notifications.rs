//! Background notifications about conversations, waiting for a free floor.
//!
//! The queue holds at most one entry per conversation: a newer event
//! replaces an older one, so a conversation that finishes twice before the
//! floor frees up is reported once. Reading a conversation
//! (`get_conversation`) acknowledges its entry — the model knows already.

/// What happened in a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationKind {
    /// The agent finished its turn.
    Finished,
    /// The agent stopped with an error.
    Failed { message: String },
    /// The agent waits for a permission decision.
    NeedsPermission { tool: String },
    /// The agent asked the user questions.
    NeedsAnswer,
}

impl NotificationKind {
    /// Whether the conversation waits for the user (as opposed to having
    /// ended its turn).
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::NeedsPermission { .. } | Self::NeedsAnswer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub conversation_id: String,
    pub name: String,
    pub project: String,
    pub kind: NotificationKind,
}

#[derive(Debug, Default)]
pub struct NotificationQueue {
    /// Arrival order, one entry per conversation.
    entries: Vec<Notification>,
}

impl NotificationQueue {
    pub fn push(&mut self, notification: Notification) {
        self.entries
            .retain(|entry| entry.conversation_id != notification.conversation_id);
        self.entries.push(notification);
    }

    /// Drop the entry of a conversation the model has just read.
    pub fn acknowledge(&mut self, conversation_id: &str) {
        self.entries
            .retain(|entry| entry.conversation_id != conversation_id);
    }

    /// Drop a conversation's "waiting for you" entry once the wait is over
    /// (answered elsewhere). A finish or failure entry stays.
    pub fn retract_waiting(&mut self, conversation_id: &str) {
        self.entries
            .retain(|entry| entry.conversation_id != conversation_id || !entry.kind.is_waiting());
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Empty the queue into one message for the model, or `None` when
    /// nothing is queued.
    pub fn take_message(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let entries = std::mem::take(&mut self.entries);
        Some(render(&entries))
    }
}

fn render(entries: &[Notification]) -> String {
    let lines: Vec<String> = entries
        .iter()
        .map(|entry| {
            let what = match &entry.kind {
                NotificationKind::Finished => "finished its turn".to_string(),
                NotificationKind::Failed { message } => {
                    format!("stopped with an error ({message})")
                }
                NotificationKind::NeedsPermission { tool } => {
                    format!("is waiting for permission to run `{tool}`")
                }
                NotificationKind::NeedsAnswer => "is waiting for answers to its questions".into(),
            };
            let project = if entry.project.is_empty() {
                String::new()
            } else {
                format!(" in project {}", entry.project)
            };
            format!(
                "- Conversation \"{}\"{project} (id {}) {what}.",
                entry.name, entry.conversation_id
            )
        })
        .collect();
    format!(
        "[background notification]\n{}\n\nThe user is not speaking right now. If it is useful \
         to them, mention this briefly. Call get_conversation before you summarise a result. \
         Otherwise say nothing.",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, kind: NotificationKind) -> Notification {
        Notification {
            conversation_id: id.into(),
            name: format!("name {id}"),
            project: "proj".into(),
            kind,
        }
    }

    #[test]
    fn newer_event_of_a_conversation_replaces_the_older_one() {
        let mut queue = NotificationQueue::default();
        queue.push(note("a", NotificationKind::NeedsAnswer));
        queue.push(note("b", NotificationKind::Finished));
        queue.push(note("a", NotificationKind::Finished));
        assert_eq!(queue.len(), 2);
        let message = queue.take_message().unwrap();
        assert!(message.find("id b").unwrap() < message.find("id a").unwrap());
        assert!(!message.contains("questions"));
        assert!(queue.is_empty());
    }

    #[test]
    fn reading_a_conversation_acknowledges_it() {
        let mut queue = NotificationQueue::default();
        queue.push(note("a", NotificationKind::Finished));
        queue.acknowledge("a");
        assert_eq!(queue.take_message(), None);
    }

    #[test]
    fn retracting_a_wait_keeps_a_finish() {
        let mut queue = NotificationQueue::default();
        queue.push(note(
            "a",
            NotificationKind::NeedsPermission {
                tool: "execute_command".into(),
            },
        ));
        queue.push(note("b", NotificationKind::Finished));
        queue.retract_waiting("a");
        queue.retract_waiting("b");
        assert_eq!(queue.len(), 1);
        assert!(queue.take_message().unwrap().contains("id b"));
    }
}
