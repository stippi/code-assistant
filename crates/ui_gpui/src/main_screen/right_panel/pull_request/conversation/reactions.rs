//! Reactions as the bar under a comment shows them, ported from T3 Code
//! (`pullRequestReactions.ts`): emoji and names, the tooltip naming who
//! reacted, and the optimistic overlay. A click shows at once; the overlay
//! holds the viewer's pending choices until the host reports different
//! reactions (its signature changes), and a failed write takes its choice
//! back.

use github::model::{Reaction, ReactionContent};
use std::collections::HashMap;

pub fn emoji(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "👍",
        ReactionContent::ThumbsDown => "👎",
        ReactionContent::Laugh => "😄",
        ReactionContent::Hooray => "🎉",
        ReactionContent::Confused => "😕",
        ReactionContent::Heart => "❤️",
        ReactionContent::Rocket => "🚀",
        ReactionContent::Eyes => "👀",
    }
}

pub fn name(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "thumbs up",
        ReactionContent::ThumbsDown => "thumbs down",
        ReactionContent::Laugh => "laugh",
        ReactionContent::Hooray => "hooray",
        ReactionContent::Confused => "confused",
        ReactionContent::Heart => "heart",
        ReactionContent::Rocket => "rocket",
        ReactionContent::Eyes => "eyes",
    }
}

/// People a tooltip names before it counts the rest.
const NAMED_ACTOR_LIMIT: usize = 3;

/// "You, ann, and 2 others reacted with heart emoji". The host leaves the
/// viewer out of `actors`; "You" is added only while the count has room.
pub fn tooltip(reaction: &Reaction) -> String {
    let count = reaction.count as usize;
    let viewer_has_room = reaction.actors.len() < count;
    let names = (reaction.viewer_has_reacted && viewer_has_room)
        .then_some("You")
        .into_iter()
        .chain(reaction.actors.iter().map(String::as_str));
    let mut parts: Vec<String> = names
        .take(NAMED_ACTOR_LIMIT.min(count))
        .map(str::to_owned)
        .collect();
    let others = count.saturating_sub(parts.len());
    if others > 0 {
        let unit = match (parts.is_empty(), others == 1) {
            (false, true) => "other",
            (false, false) => "others",
            (true, true) => "person",
            (true, false) => "people",
        };
        parts.push(format!("{others} {unit}"));
    }
    format!(
        "{} reacted with {} emoji",
        join_names(&parts),
        name(reaction.content)
    )
}

/// "a", "a and b", "a, b, and c".
fn join_names(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// Changes whenever the host reports different reactions.
pub fn signature(reactions: &[Reaction]) -> String {
    reactions
        .iter()
        .map(|r| {
            format!(
                "{:?}:{}:{}",
                r.content,
                r.count,
                u8::from(r.viewer_has_reacted)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `reactions` with the viewer's pending choices applied, in GitHub's order.
pub fn apply_pending(
    reactions: &[Reaction],
    pending: &HashMap<ReactionContent, bool>,
) -> Vec<Reaction> {
    if pending.is_empty() {
        return reactions.to_vec();
    }
    let mut by_content: HashMap<ReactionContent, Reaction> =
        reactions.iter().map(|r| (r.content, r.clone())).collect();
    for (&content, &reacted) in pending {
        match by_content.get_mut(&content) {
            None if reacted => {
                by_content.insert(
                    content,
                    Reaction {
                        content,
                        count: 1,
                        actors: Vec::new(),
                        viewer_has_reacted: true,
                    },
                );
            }
            None => {}
            Some(current) if current.viewer_has_reacted == reacted => {}
            Some(current) => {
                if reacted {
                    current.count += 1;
                } else {
                    current.count = current.count.saturating_sub(1);
                }
                current.viewer_has_reacted = reacted;
                if current.count == 0 {
                    by_content.remove(&content);
                }
            }
        }
    }
    ReactionContent::ALL
        .into_iter()
        .filter_map(|content| by_content.remove(&content))
        .collect()
}

/// The viewer's reactions on one subject that the host has not reported
/// yet.
#[derive(Clone, Debug, Default)]
pub struct PendingReactions {
    /// The host's reactions the choices were made on.
    signature: String,
    choices: HashMap<ReactionContent, bool>,
}

impl PendingReactions {
    /// The choices still in force on the host's `reactions`: none once the
    /// host reports something else than they were made on.
    fn current(&self, reactions: &[Reaction]) -> Option<&HashMap<ReactionContent, bool>> {
        (self.signature == signature(reactions)).then_some(&self.choices)
    }

    /// What the bar shows for the host's `reactions`.
    pub fn shown(&self, reactions: &[Reaction]) -> Vec<Reaction> {
        match self.current(reactions) {
            Some(choices) => apply_pending(reactions, choices),
            None => reactions.to_vec(),
        }
    }

    /// The viewer (un)reacts with `content` on the host's `reactions`.
    pub fn choose(&mut self, reactions: &[Reaction], content: ReactionContent, reacted: bool) {
        if self.current(reactions).is_none() {
            self.signature = signature(reactions);
            self.choices.clear();
        }
        self.choices.insert(content, reacted);
    }

    /// The write of `content` failed: show what the host has.
    pub fn revert(&mut self, content: ReactionContent) {
        self.choices.remove(&content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReactionContent::*;

    fn reaction(content: ReactionContent, count: u32, actors: &[&str], mine: bool) -> Reaction {
        Reaction {
            content,
            count,
            actors: actors.iter().map(|a| a.to_string()).collect(),
            viewer_has_reacted: mine,
        }
    }

    #[test]
    fn tooltips_name_a_few_and_count_the_rest() {
        assert_eq!(
            tooltip(&reaction(Heart, 1, &[], true)),
            "You reacted with heart emoji"
        );
        assert_eq!(
            tooltip(&reaction(ThumbsUp, 2, &["ann"], true)),
            "You and ann reacted with thumbs up emoji"
        );
        assert_eq!(
            tooltip(&reaction(Rocket, 5, &["ann", "bob", "cid"], true)),
            "You, ann, bob, and 2 others reacted with rocket emoji"
        );
        // Ten actors are reported at most; the viewer has no room among them.
        assert_eq!(
            tooltip(&reaction(Eyes, 2, &["ann", "bob"], true)),
            "ann and bob reacted with eyes emoji"
        );
        assert_eq!(
            tooltip(&reaction(Laugh, 4, &[], false)),
            "4 people reacted with laugh emoji"
        );
        assert_eq!(
            tooltip(&reaction(Laugh, 1, &[], false)),
            "1 person reacted with laugh emoji"
        );
    }

    #[test]
    fn signatures_follow_count_and_viewer() {
        let a = [
            reaction(ThumbsUp, 1, &[], false),
            reaction(Heart, 2, &[], true),
        ];
        assert_eq!(signature(&a), "ThumbsUp:1:0 Heart:2:1");
        assert_ne!(
            signature(&a),
            signature(&[reaction(ThumbsUp, 1, &[], false)])
        );
    }

    #[test]
    fn pending_choices_overlay_the_hosts_reactions() {
        let host = [
            reaction(Heart, 2, &["ann"], true),
            reaction(ThumbsUp, 1, &["bob"], false),
        ];
        let pending = HashMap::from([
            (ThumbsUp, true),
            (Heart, false),
            (Rocket, true),
            (Eyes, false),
        ]);
        assert_eq!(
            apply_pending(&host, &pending),
            [
                reaction(ThumbsUp, 2, &["bob"], true),
                reaction(Heart, 1, &["ann"], false),
                reaction(Rocket, 1, &[], true),
            ]
        );
        // The last reactor leaving drops the pill.
        let mine = [reaction(Eyes, 1, &[], true)];
        assert!(apply_pending(&mine, &HashMap::from([(Eyes, false)])).is_empty());
        // A choice the host already reflects changes nothing.
        assert_eq!(apply_pending(&mine, &HashMap::from([(Eyes, true)])), mine);
    }

    #[test]
    fn choices_hold_until_the_host_reports_new_reactions() {
        let host = vec![reaction(Heart, 1, &["ann"], false)];
        let mut pending = PendingReactions::default();
        pending.choose(&host, Heart, true);
        assert_eq!(pending.shown(&host), [reaction(Heart, 2, &["ann"], true)]);
        pending.choose(&host, Rocket, true);
        assert_eq!(pending.shown(&host).len(), 2);

        // The host's next read carries the reaction: the overlay is spent.
        let refreshed = vec![reaction(Heart, 2, &["ann"], true)];
        assert_eq!(pending.shown(&refreshed), refreshed);
        // A choice on the new reactions starts over.
        pending.choose(&refreshed, Heart, false);
        assert_eq!(
            pending.shown(&refreshed),
            [reaction(Heart, 1, &["ann"], false)]
        );
    }

    #[test]
    fn a_failed_write_takes_its_choice_back() {
        let host = vec![reaction(Heart, 1, &["ann"], false)];
        let mut pending = PendingReactions::default();
        pending.choose(&host, Heart, true);
        pending.choose(&host, Eyes, true);
        pending.revert(Heart);
        assert_eq!(
            pending.shown(&host),
            [
                reaction(Heart, 1, &["ann"], false),
                reaction(Eyes, 1, &[], true)
            ]
        );
    }
}
