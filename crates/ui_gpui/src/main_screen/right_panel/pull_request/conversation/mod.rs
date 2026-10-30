//! The building blocks of a pull request's conversation, shared by the
//! Summary, Timeline and Code tabs:
//!
//! - [`logic`]: loose comments and threads as one sequence, the comment
//!   filter, paging, reviewers' verdicts (pure, tested);
//! - [`reactions`]: emoji, tooltips and the optimistic overlay (pure);
//! - [`CommentCard`]: one comment with its header, verdict, reactions,
//!   edit-in-place and clipped body;
//! - [`ThreadCard`]: a review thread with resolve, replies, "Load more
//!   comments" and a reply box, folded to one line once resolved;
//! - [`MarkdownEditor`]: the Write | Preview box every comment is written
//!   in (⌘↵ submits, Escape cancels).
//!
//! Cards are entities owned by the tab that shows them, kept per comment
//! (or thread) id across refreshes; they write through
//! [`PullRequestData::write`](super::data::PullRequestData::write) and
//! emit [`PullRequestIntent`](super::PullRequestIntent)s (links to open).

pub mod body;
pub mod card;
pub mod editor;
pub mod logic;
pub mod parts;
pub mod reaction_bar;
pub mod reactions;
pub mod thread;

pub use body::CommentBody;
pub use card::{CardStyle, CommentCard};
pub use editor::{EditorEvent, EditorOptions, MarkdownEditor};
pub use parts::{Location, outcome_color, outcome_icon};
pub use thread::ThreadCard;

#[cfg(test)]
pub mod fixtures;
