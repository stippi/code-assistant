//! What the list shows before GitHub confirms it: a row closed, merged,
//! reopened or marked ready shows so at once, and keeps that state until a
//! listing agrees (or a minute passed, when the host's answer wins). A
//! failed action takes its override back. Line counts read for rows that
//! came without them fill those rows in. Ported from T3 Code's
//! `pullRequestList.logic.ts` (`pullRequestOverrideAfterAction`,
//! `applyPullRequestOverrides`, `settlePullRequestOverrides`,
//! `pullRequestQuickActions`, `withDiffStat`).

use chrono::{DateTime, Duration, Utc};
use github::model::{
    Action, DiffStat, ListEntry, ListState, MergeMethod, PullRequestAction, PullRequestState,
};
use std::collections::HashMap;

/// How long an override outranks a listing that disagrees with it.
const OVERRIDE_TRUST: Duration = Duration::seconds(60);

/// A row's state as the viewer's action left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowOverride {
    pub state: PullRequestState,
    /// Set by draft and ready; the others keep the row's.
    pub is_draft: Option<bool>,
    /// When the action ran; the row shows it as its update.
    pub at: DateTime<Utc>,
    /// Tells this override from a later one on the same row.
    pub token: u64,
}

/// The override `action` leaves on a row in `state`; `None` for actions
/// that change nothing the list shows.
pub fn override_after_action(
    state: PullRequestState,
    action: PullRequestAction,
    now: DateTime<Utc>,
    token: u64,
) -> Option<RowOverride> {
    let (state, is_draft) = match action {
        PullRequestAction::Close => (PullRequestState::Closed, None),
        PullRequestAction::Reopen => (PullRequestState::Open, None),
        PullRequestAction::Merge => (PullRequestState::Merged, None),
        PullRequestAction::Draft => (state, Some(true)),
        PullRequestAction::Ready => (state, Some(false)),
        _ => return None,
    };
    Some(RowOverride {
        state,
        is_draft,
        at: now,
        token,
    })
}

/// `entries` with their overrides applied; rows an override moved out of
/// the listed `state` leave.
pub fn apply_overrides(
    entries: &[ListEntry],
    overrides: &HashMap<u64, RowOverride>,
    listed: ListState,
) -> Vec<ListEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            let mut entry = entry.clone();
            if let Some(o) = overrides.get(&entry.number) {
                entry.state = o.state;
                if let Some(is_draft) = o.is_draft {
                    entry.is_draft = is_draft;
                }
                entry.updated_at = o.at;
            }
            in_listed_state(entry.state, listed).then_some(entry)
        })
        .collect()
}

fn in_listed_state(state: PullRequestState, listed: ListState) -> bool {
    match listed {
        ListState::All => true,
        ListState::Open => state == PullRequestState::Open,
        ListState::Closed => state == PullRequestState::Closed,
        ListState::Merged => state == PullRequestState::Merged,
    }
}

/// Retire the overrides a listing agrees with, and those it outranks by
/// now. A row missing from the listing proves nothing: its override stays.
pub fn settle_overrides(
    overrides: &mut HashMap<u64, RowOverride>,
    answered: &[ListEntry],
    now: DateTime<Utc>,
) {
    overrides.retain(|number, o| {
        let Some(row) = answered.iter().find(|e| e.number == *number) else {
            return true;
        };
        let agrees = row.state == o.state && o.is_draft.is_none_or(|d| d == row.is_draft);
        !agrees && now - o.at <= OVERRIDE_TRUST
    });
}

/// What a row offers while Shift is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuickAction {
    Close,
    Merge,
    Ready,
    Reopen,
}

impl QuickAction {
    pub fn label(self) -> &'static str {
        match self {
            QuickAction::Close => "Close",
            QuickAction::Merge => "Merge",
            QuickAction::Ready => "Ready",
            QuickAction::Reopen => "Reopen",
        }
    }

    /// Closing and merging take a confirming click.
    pub fn confirms(self) -> bool {
        matches!(self, QuickAction::Close | QuickAction::Merge)
    }

    pub fn action(self, method: MergeMethod) -> Action {
        match self {
            QuickAction::Close => Action::Close,
            QuickAction::Merge => Action::Merge(method),
            QuickAction::Ready => Action::Ready,
            QuickAction::Reopen => Action::Reopen,
        }
    }
}

pub fn quick_actions(state: PullRequestState, is_draft: bool) -> &'static [QuickAction] {
    match state {
        PullRequestState::Merged => &[],
        PullRequestState::Closed => &[QuickAction::Reopen],
        PullRequestState::Open if is_draft => &[QuickAction::Close, QuickAction::Ready],
        PullRequestState::Open => &[QuickAction::Close, QuickAction::Merge],
    }
}

/// Fill in line counts for rows that came without them (0/0); counts a
/// row brought itself win.
pub fn with_stats(entries: &mut [ListEntry], stats: &HashMap<u64, (u64, u64)>) {
    for entry in entries {
        if entry.additions == 0
            && entry.deletions == 0
            && let Some(&(additions, deletions)) = stats.get(&entry.number)
        {
            entry.additions = additions;
            entry.deletions = deletions;
        }
    }
}

/// Merge newly read counts into those held.
pub fn merge_stats(held: &mut HashMap<u64, (u64, u64)>, read: &[DiffStat]) {
    for stat in read {
        held.insert(stat.pr.number, (stat.additions, stat.deletions));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{entry, entry_with, now, repo};

    fn numbers(entries: &[ListEntry]) -> Vec<u64> {
        entries.iter().map(|e| e.number).collect()
    }

    #[test]
    fn actions_leave_overrides() {
        let closed =
            override_after_action(PullRequestState::Open, PullRequestAction::Close, now(), 1);
        assert_eq!(closed.map(|o| o.state), Some(PullRequestState::Closed));
        let ready =
            override_after_action(PullRequestState::Open, PullRequestAction::Ready, now(), 2)
                .unwrap();
        assert_eq!(
            (ready.state, ready.is_draft),
            (PullRequestState::Open, Some(false))
        );
        let merged =
            override_after_action(PullRequestState::Open, PullRequestAction::Merge, now(), 3);
        assert_eq!(merged.map(|o| o.state), Some(PullRequestState::Merged));
        assert_eq!(
            override_after_action(
                PullRequestState::Open,
                PullRequestAction::UpdateBranch,
                now(),
                4
            ),
            None
        );
    }

    #[test]
    fn overrides_show_at_once_and_move_rows_out_of_the_listed_state() {
        let entries = [
            entry(1, "a", "ann"),
            entry_with(2, "b", "bob", |e| e.is_draft = true),
        ];
        let mut overrides = HashMap::new();
        overrides.insert(
            1,
            override_after_action(PullRequestState::Open, PullRequestAction::Close, now(), 1)
                .unwrap(),
        );
        overrides.insert(
            2,
            override_after_action(PullRequestState::Open, PullRequestAction::Ready, now(), 2)
                .unwrap(),
        );
        let open = apply_overrides(&entries, &overrides, ListState::Open);
        assert_eq!(numbers(&open), [2]);
        assert!(!open[0].is_draft);
        assert_eq!(open[0].updated_at, now());
        let all = apply_overrides(&entries, &overrides, ListState::All);
        assert_eq!(all[0].state, PullRequestState::Closed);
    }

    #[test]
    fn overrides_hold_until_a_listing_agrees_or_a_minute_passed() {
        let mut overrides = HashMap::new();
        let close =
            override_after_action(PullRequestState::Open, PullRequestAction::Close, now(), 1)
                .unwrap();
        overrides.insert(1, close.clone());
        overrides.insert(
            2,
            RowOverride {
                token: 2,
                ..close.clone()
            },
        );
        overrides.insert(3, RowOverride { token: 3, ..close });

        // #1 still open on the host, #2 closed, #3 not listed.
        let answered = [
            entry(1, "a", "ann"),
            entry_with(2, "b", "ann", |e| e.state = PullRequestState::Closed),
        ];
        settle_overrides(&mut overrides, &answered, now() + Duration::seconds(10));
        let mut kept: Vec<_> = overrides.keys().copied().collect();
        kept.sort();
        assert_eq!(kept, [1, 3]);

        settle_overrides(&mut overrides, &answered, now() + Duration::seconds(61));
        let kept: Vec<_> = overrides.keys().copied().collect();
        assert_eq!(kept, [3], "the host wins after a minute");
    }

    #[test]
    fn rows_offer_what_their_state_allows() {
        use QuickAction::*;
        assert_eq!(quick_actions(PullRequestState::Open, false), [Close, Merge]);
        assert_eq!(quick_actions(PullRequestState::Open, true), [Close, Ready]);
        assert_eq!(quick_actions(PullRequestState::Closed, false), [Reopen]);
        assert!(quick_actions(PullRequestState::Merged, false).is_empty());
        assert!(Close.confirms() && Merge.confirms() && !Ready.confirms());
        assert_eq!(
            Merge.action(MergeMethod::Squash),
            Action::Merge(MergeMethod::Squash)
        );
    }

    #[test]
    fn read_counts_fill_rows_without_their_own() {
        let mut held = HashMap::new();
        merge_stats(
            &mut held,
            &[
                DiffStat {
                    pr: repo().pull_request(1),
                    additions: 12,
                    deletions: 3,
                },
                DiffStat {
                    pr: repo().pull_request(2),
                    additions: 99,
                    deletions: 99,
                },
            ],
        );
        let mut entries = [
            entry(1, "a", "ann"),
            entry_with(2, "b", "ann", |e| e.additions = 5),
        ];
        with_stats(&mut entries, &held);
        assert_eq!((entries[0].additions, entries[0].deletions), (12, 3));
        assert_eq!((entries[1].additions, entries[1].deletions), (5, 0));
    }
}
