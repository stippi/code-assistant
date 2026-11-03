//! What the conversation views remember across restarts (with the pull
//! request view's other [preferences](super::super::preferences)): the
//! Summary's comment filter and order, and the Timeline's order.

use super::logic::CommentFilter;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationPreferences {
    pub show_bot_comments: bool,
    /// Resolved threads and dismissed reviews.
    pub show_resolved_comments: bool,
    pub comments_oldest_first: bool,
    pub timeline_oldest_first: bool,
}

impl ConversationPreferences {
    pub fn comment_filter(&self) -> CommentFilter {
        CommentFilter {
            bots: self.show_bot_comments,
            resolved: self.show_resolved_comments,
        }
    }

    pub fn set_comment_filter(&mut self, filter: CommentFilter) {
        self.show_bot_comments = filter.bots;
        self.show_resolved_comments = filter.resolved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_round_trips_and_defaults_to_hiding() {
        let mut preferences = ConversationPreferences::default();
        assert_eq!(preferences.comment_filter(), CommentFilter::default());
        let filter = CommentFilter {
            bots: true,
            resolved: false,
        };
        preferences.set_comment_filter(filter);
        assert_eq!(preferences.comment_filter(), filter);
        let json = serde_json::to_string(&preferences).unwrap();
        assert_eq!(
            serde_json::from_str::<ConversationPreferences>(&json).unwrap(),
            preferences
        );
        assert_eq!(
            serde_json::from_str::<ConversationPreferences>("{}").unwrap(),
            ConversationPreferences::default()
        );
    }
}
