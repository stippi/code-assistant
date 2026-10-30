//! How the list arranges a repository's pull requests, apart from any view:
//! what the search field's qualifiers mean, which rows the filters keep,
//! how rows group by the viewer's part in them and how each group sorts.
//! Ported from T3 Code's `pullRequestList.logic.ts` and `pullRequestQuery.ts`.
//!
//! State and involvement narrow the listing on the host ([`ListQuery`]);
//! the free text is searched there too, so a row may match by its
//! description and is only ranked here, never dropped. Everything else
//! (`label:`, `author:`, `draft:`, `review:`, `checks:` and the Filters
//! menu) filters the loaded rows.
//!
//! [`ListQuery`]: github::model::ListQuery

use github::model::{
    ChecksState, Involvement, Label, ListEntry, Mergeability, PullRequestState, ReviewDecision,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Qualifier values (labels in one `label:`, label groups) read at most.
const MAX_QUALIFIER_VALUES: usize = 10;
/// Characters of one qualifier value kept.
const MAX_QUALIFIER_LENGTH: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortMode {
    #[default]
    MergeReadiness,
    BlockedOnMe,
    RecentlyUpdated,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

impl SortMode {
    pub const ALL: [SortMode; 7] = [
        SortMode::MergeReadiness,
        SortMode::BlockedOnMe,
        SortMode::RecentlyUpdated,
        SortMode::Newest,
        SortMode::Oldest,
        SortMode::Largest,
        SortMode::Smallest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::MergeReadiness => "Merge readiness",
            SortMode::BlockedOnMe => "Blocked on me",
            SortMode::RecentlyUpdated => "Recently updated",
            SortMode::Newest => "Newest shown",
            SortMode::Oldest => "Oldest shown",
            SortMode::Largest => "Largest shown",
            SortMode::Smallest => "Smallest shown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftFilter {
    Only,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewFilter {
    Approved,
    ChangesRequested,
    ReviewRequired,
    /// No review decision at all.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksFilter {
    Passing,
    Failing,
}

/// What the loaded rows are narrowed to, from the Filters menu or typed
/// qualifiers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub draft: Option<DraftFilter>,
    pub review: Option<ReviewFilter>,
    pub checks: Option<ChecksFilter>,
    /// Every group must match; any label of a group suffices.
    pub labels: Vec<Vec<String>>,
    pub excluded_labels: Vec<String>,
    /// A login; `@me` and `me` name the viewer.
    pub author: Option<String>,
}

impl Filters {
    /// These filters with each one `typed` sets taken from it instead:
    /// a qualifier in the search field outranks the menu.
    pub fn overlaid(&self, typed: &Filters) -> Filters {
        fn pick<T: Clone>(typed: &[T], menu: &[T]) -> Vec<T> {
            if typed.is_empty() { menu } else { typed }.to_vec()
        }
        Filters {
            draft: typed.draft.or(self.draft),
            review: typed.review.or(self.review),
            checks: typed.checks.or(self.checks),
            labels: pick(&typed.labels, &self.labels),
            excluded_labels: pick(&typed.excluded_labels, &self.excluded_labels),
            author: typed.author.clone().or_else(|| self.author.clone()),
        }
    }
}

/// The search field's text split into free text and qualifiers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub text: String,
    pub filters: Filters,
}

pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut text: Vec<&str> = Vec::new();
    let mut filters = Filters::default();
    for token in query_tokens(raw) {
        if !apply_qualifier(token, &mut filters) {
            text.push(token);
        }
    }
    filters.labels.truncate(MAX_QUALIFIER_VALUES);
    filters.excluded_labels.truncate(MAX_QUALIFIER_VALUES);
    ParsedQuery {
        text: text.join(" "),
        filters,
    }
}

/// Whitespace-separated tokens; a quoted stretch keeps its spaces. A quote
/// without its partner ends the token and is dropped.
fn query_tokens(raw: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut chars = raw.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let closing = (c == '"').then(|| raw[i + 1..].find('"')).flatten();
        if c.is_whitespace() || (c == '"' && closing.is_none()) {
            if let Some(s) = start.take() {
                tokens.push(&raw[s..i]);
            }
            continue;
        }
        start.get_or_insert(i);
        if let Some(offset) = closing {
            let end = i + 1 + offset;
            while chars.peek().is_some_and(|&(j, _)| j <= end) {
                chars.next();
            }
        }
    }
    if let Some(s) = start {
        tokens.push(&raw[s..]);
    }
    tokens
}

/// `-?key:value` with a key of letters, digits, `_` and `-`.
fn split_qualifier(token: &str) -> Option<(bool, &str, &str)> {
    let (negated, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let (key, value) = rest.split_once(':')?;
    let mut key_chars = key.chars();
    let valid = key_chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && key_chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then_some((negated, key, value))
}

fn unquoted(raw: &str) -> String {
    raw.replace('"', "").trim().to_owned()
}

/// The names of a comma-separated qualifier list, quotes keeping commas.
fn qualifier_names(raw: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in raw.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => names.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    names.push(current);
    names
        .into_iter()
        .map(|name| {
            name.trim()
                .chars()
                .take(MAX_QUALIFIER_LENGTH)
                .collect::<String>()
        })
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .take(MAX_QUALIFIER_VALUES)
        .collect()
}

/// Reads `token` into `filters` when it is a qualifier this list knows;
/// `false` leaves it free text.
fn apply_qualifier(token: &str, filters: &mut Filters) -> bool {
    let Some((negated, key, raw_value)) = split_qualifier(token) else {
        return false;
    };
    let value = unquoted(raw_value);
    if value.is_empty() {
        return false;
    }
    let mut add_labels = |names: Vec<String>| {
        if names.is_empty() {
            return false;
        }
        if negated {
            filters.excluded_labels.extend(names);
        } else {
            filters.labels.push(names);
        }
        true
    };
    match key.to_ascii_lowercase().as_str() {
        "label" => add_labels(qualifier_names(raw_value)),
        "author" if !negated => {
            filters.author = Some(value.chars().take(MAX_QUALIFIER_LENGTH).collect());
            true
        }
        "draft" if !negated => match value.to_ascii_lowercase().as_str() {
            "true" => {
                filters.draft = Some(DraftFilter::Only);
                true
            }
            "false" => {
                filters.draft = Some(DraftFilter::Hide);
                true
            }
            _ => false,
        },
        "review" if !negated => {
            let review = match value.to_ascii_lowercase().as_str() {
                "approved" => ReviewFilter::Approved,
                "changes_requested" | "changes-requested" => ReviewFilter::ChangesRequested,
                "required" | "review-required" => ReviewFilter::ReviewRequired,
                "none" => ReviewFilter::None,
                _ => return false,
            };
            filters.review = Some(review);
            true
        }
        "status" | "checks" if !negated => {
            let checks = match value.to_ascii_lowercase().as_str() {
                "success" | "passing" => ChecksFilter::Passing,
                "failure" | "failing" => ChecksFilter::Failing,
                _ => return false,
            };
            filters.checks = Some(checks);
            true
        }
        "author" | "draft" | "review" | "status" | "checks" => false,
        // Anything else is a scoped label (`area:ui`), unless it reads like a
        // path or URL.
        _ if !value.starts_with('/') => add_labels(
            qualifier_names(raw_value)
                .into_iter()
                .map(|name| {
                    if name.contains(':') {
                        name
                    } else {
                        format!("{key}:{name}")
                    }
                })
                .collect(),
        ),
        _ => false,
    }
}

/// Whether `entry` passes `filters`; `viewer` resolves `author:@me`.
pub fn matches_filters(entry: &ListEntry, filters: &Filters, viewer: Option<&str>) -> bool {
    let holds = |name: &str| {
        let name = name.trim();
        entry
            .labels
            .iter()
            .any(|label| label.name.trim().eq_ignore_ascii_case(name))
    };
    let draft = match filters.draft {
        None => true,
        Some(DraftFilter::Only) => entry.is_draft,
        Some(DraftFilter::Hide) => !entry.is_draft,
    };
    let review = match filters.review {
        None => true,
        Some(ReviewFilter::None) => entry.review_decision.is_none(),
        Some(ReviewFilter::Approved) => entry.review_decision == Some(ReviewDecision::Approved),
        Some(ReviewFilter::ChangesRequested) => {
            entry.review_decision == Some(ReviewDecision::ChangesRequested)
        }
        Some(ReviewFilter::ReviewRequired) => {
            entry.review_decision == Some(ReviewDecision::ReviewRequired)
        }
    };
    let checks = match filters.checks {
        None => true,
        Some(ChecksFilter::Passing) => entry.checks_state == Some(ChecksState::Passing),
        Some(ChecksFilter::Failing) => entry.checks_state == Some(ChecksState::Failing),
    };
    let author = filters.author.as_deref().is_none_or(|author| {
        let author = resolve_author(author, viewer);
        entry
            .author
            .as_ref()
            .is_some_and(|actor| actor.login.eq_ignore_ascii_case(author))
    });
    draft
        && review
        && checks
        && author
        && filters
            .labels
            .iter()
            .all(|group| group.iter().any(|name| holds(name)))
        && !filters.excluded_labels.iter().any(|name| holds(name))
}

/// `@me` and `me` name the viewer, when known.
fn resolve_author<'a>(author: &'a str, viewer: Option<&'a str>) -> &'a str {
    let author = author.trim();
    let names_viewer = author.eq_ignore_ascii_case("me") || author.eq_ignore_ascii_case("@me");
    match viewer.map(str::trim).filter(|v| !v.is_empty()) {
        Some(viewer) if names_viewer => viewer,
        _ => author,
    }
}

/// How well `entry` matches the free text `query`; `repository` is the
/// listing's `owner/name`. Rows the host found by their description score
/// lowest but still count.
pub fn score_match(entry: &ListEntry, query: &str, repository: &str) -> u32 {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    let number = needle.strip_prefix('#').unwrap_or(&needle);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        return if entry.number.to_string() == number {
            100
        } else {
            0
        };
    }
    let title = entry.title.to_lowercase();
    let terms: Vec<&str> = needle.split_whitespace().collect();
    let author = entry
        .author
        .as_ref()
        .map(|a| a.login.to_lowercase())
        .unwrap_or_default();
    if title == needle {
        90
    } else if title.contains(&needle) {
        80
    } else if terms.len() > 1 && terms.iter().all(|term| title.contains(term)) {
        70
    } else if entry.head_branch.to_lowercase().contains(&needle) {
        60
    } else if author.contains(&needle) {
        50
    } else if repository.to_lowercase().contains(&needle) {
        40
    } else if terms.iter().any(|term| title.contains(term)) {
        30
    } else {
        10
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKey {
    Authored,
    ReviewRequested,
    Others,
}

impl GroupKey {
    pub fn label(self) -> &'static str {
        match self {
            GroupKey::Authored => "Authored",
            GroupKey::ReviewRequested => "Review requested",
            GroupKey::Others => "Others",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group<'a> {
    pub key: GroupKey,
    pub entries: Vec<&'a ListEntry>,
}

/// Everything that decides the arrangement besides the rows.
#[derive(Clone, Debug)]
pub struct Arrangement<'a> {
    pub sort: SortMode,
    /// The search field as typed.
    pub query: &'a str,
    pub menu_filters: &'a Filters,
    /// The involvement the host listed by.
    pub involvement: Involvement,
    /// The signed-in login.
    pub viewer: Option<&'a str>,
    /// `owner/name` of the listed repository.
    pub repository: &'a str,
}

/// The groups to show, in order, each sorted. `entries` come as the host
/// listed them, most recently updated first.
pub fn arrange<'a>(entries: &'a [ListEntry], arrangement: &Arrangement<'_>) -> Vec<Group<'a>> {
    let parsed = parse_query(arrangement.query);
    let filters = arrangement.menu_filters.overlaid(&parsed.filters);
    let mut kept: Vec<&ListEntry> = entries
        .iter()
        .filter(|entry| matches_filters(entry, &filters, arrangement.viewer))
        .collect();
    let searching = !parsed.text.trim().is_empty();
    if searching {
        let score = |entry: &ListEntry| score_match(entry, &parsed.text, arrangement.repository);
        kept.sort_by(|a, b| score(b).cmp(&score(a)).then(by_recency(a, b)));
    }
    let mut groups = group(kept, arrangement.viewer);
    if !searching {
        for group in &mut groups {
            sort_group(group, arrangement.sort, arrangement.involvement);
        }
    }
    groups
}

fn by_recency(a: &ListEntry, b: &ListEntry) -> Ordering {
    b.updated_at.cmp(&a.updated_at)
}

fn group<'a>(entries: Vec<&'a ListEntry>, viewer: Option<&str>) -> Vec<Group<'a>> {
    let mut groups = [
        GroupKey::Authored,
        GroupKey::ReviewRequested,
        GroupKey::Others,
    ]
    .map(|key| Group {
        key,
        entries: Vec::new(),
    });
    for entry in entries {
        let index = match viewer {
            Some(viewer) if is_author(entry, viewer) => 0,
            Some(viewer) if is_review_requested(entry, viewer) => 1,
            _ => 2,
        };
        groups[index].entries.push(entry);
    }
    groups
        .into_iter()
        .filter(|g| !g.entries.is_empty())
        .collect()
}

fn is_author(entry: &ListEntry, viewer: &str) -> bool {
    entry
        .author
        .as_ref()
        .is_some_and(|a| a.login.eq_ignore_ascii_case(viewer))
}

/// As the host's `review-requested:@me` reads it: a team request counts.
fn is_review_requested(entry: &ListEntry, viewer: &str) -> bool {
    entry.has_team_review_request
        || entry
            .review_request_logins
            .iter()
            .any(|login| login.eq_ignore_ascii_case(viewer))
}

/// Rows with line counts come before rows still without them.
fn measured(entry: &ListEntry) -> bool {
    entry.additions + entry.deletions > 0
}

fn size(entry: &ListEntry) -> u64 {
    entry.additions + entry.deletions
}

fn sort_group(group: &mut Group<'_>, sort: SortMode, involvement: Involvement) {
    let entries = &mut group.entries;
    match sort {
        SortMode::MergeReadiness => entries.sort_by(|a, b| {
            readiness_tier(a)
                .cmp(&readiness_tier(b))
                .then(measured(b).cmp(&measured(a)))
                .then(size(a).cmp(&size(b)))
                .then(by_recency(a, b))
        }),
        SortMode::BlockedOnMe => {
            let role = match group.key {
                GroupKey::Authored => Involvement::Authored,
                GroupKey::ReviewRequested => Involvement::ReviewRequested,
                GroupKey::Others => involvement,
            };
            let tier: fn(&ListEntry) -> u8 = match role {
                Involvement::All => return,
                Involvement::Authored => blocked_on_author_tier,
                Involvement::ReviewRequested => blocked_on_reviewer_tier,
            };
            entries.sort_by(|a, b| tier(a).cmp(&tier(b)).then(by_recency(a, b)));
        }
        SortMode::RecentlyUpdated => {}
        SortMode::Newest | SortMode::Oldest => {
            let created = |e: &ListEntry| e.created_at;
            entries.sort_by(|a, b| {
                let order = created(a).cmp(&created(b));
                let order = if sort == SortMode::Newest {
                    order.reverse()
                } else {
                    order
                };
                order.then(by_recency(a, b))
            })
        }
        SortMode::Largest | SortMode::Smallest => entries.sort_by(|a, b| {
            let order = size(a).cmp(&size(b));
            let order = if sort == SortMode::Largest {
                order.reverse()
            } else {
                order
            };
            measured(b)
                .cmp(&measured(a))
                .then(order)
                .then(by_recency(a, b))
        }),
    }
}

fn readiness_tier(entry: &ListEntry) -> u8 {
    let passing = entry.checks_state == Some(ChecksState::Passing);
    if entry.mergeability == Mergeability::Conflicting {
        4
    } else if entry.state != PullRequestState::Open {
        3
    } else if entry.is_draft {
        2
    } else if passing && entry.review_decision == Some(ReviewDecision::Approved) {
        0
    } else if passing {
        1
    } else {
        2
    }
}

/// What the author has to do first: conflicts, requested changes, failing
/// checks, a draft; finished work last.
fn blocked_on_author_tier(entry: &ListEntry) -> u8 {
    if entry.state != PullRequestState::Open {
        6
    } else if entry.mergeability == Mergeability::Conflicting {
        0
    } else if entry.review_decision == Some(ReviewDecision::ChangesRequested) {
        1
    } else if entry.checks_state == Some(ChecksState::Failing) {
        2
    } else if entry.is_draft {
        3
    } else if entry.checks_state == Some(ChecksState::Passing)
        && entry.review_decision == Some(ReviewDecision::Approved)
    {
        5
    } else {
        4
    }
}

fn blocked_on_reviewer_tier(entry: &ListEntry) -> u8 {
    u8::from(entry.state != PullRequestState::Open)
}

/// How many loaded rows each author and label has, most first; the Author
/// and Labels filters offer them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facets {
    pub authors: Vec<(String, usize)>,
    pub labels: Vec<(Label, usize)>,
}

pub fn facets(entries: &[ListEntry]) -> Facets {
    let mut authors: Vec<(String, usize)> = Vec::new();
    let mut labels: Vec<(Label, usize)> = Vec::new();
    for entry in entries {
        if let Some(author) = &entry.author {
            match authors
                .iter_mut()
                .find(|(login, _)| login.eq_ignore_ascii_case(&author.login))
            {
                Some((_, count)) => *count += 1,
                None => authors.push((author.login.clone(), 1)),
            }
        }
        for label in &entry.labels {
            match labels
                .iter_mut()
                .find(|(held, _)| held.name.trim().eq_ignore_ascii_case(label.name.trim()))
            {
                Some((_, count)) => *count += 1,
                None => labels.push((label.clone(), 1)),
            }
        }
    }
    authors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    labels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    Facets { authors, labels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{entry, entry_with, hours_ago};
    use github::model::Label;

    fn label(name: &str) -> Label {
        Label {
            name: name.into(),
            color: None,
        }
    }

    fn numbers(groups: &[Group]) -> Vec<(GroupKey, Vec<u64>)> {
        groups
            .iter()
            .map(|g| (g.key, g.entries.iter().map(|e| e.number).collect()))
            .collect()
    }

    fn arranged(entries: &[ListEntry], sort: SortMode, query: &str) -> Vec<(GroupKey, Vec<u64>)> {
        let filters = Filters::default();
        numbers(&arrange(
            entries,
            &Arrangement {
                sort,
                query,
                menu_filters: &filters,
                involvement: Involvement::All,
                viewer: Some("me"),
                repository: "stippi/code-assistant",
            },
        ))
    }

    #[test]
    fn plain_text_is_free_text() {
        let parsed = parse_query("  image   renderer ");
        assert_eq!(parsed.text, "image renderer");
        assert_eq!(parsed.filters, Filters::default());
    }

    #[test]
    fn qualifiers_become_filters() {
        let parsed = parse_query(
            "fix label:bug,\"good first issue\" -label:wontfix author:@me draft:false review:approved checks:failure",
        );
        assert_eq!(parsed.text, "fix");
        assert_eq!(
            parsed.filters,
            Filters {
                draft: Some(DraftFilter::Hide),
                review: Some(ReviewFilter::Approved),
                checks: Some(ChecksFilter::Failing),
                labels: vec![vec!["bug".into(), "good first issue".into()]],
                excluded_labels: vec!["wontfix".into()],
                author: Some("@me".into()),
            }
        );
    }

    #[test]
    fn invalid_qualifier_values_stay_text() {
        let parsed = parse_query("draft:maybe review:soon -author:x checks: status:success");
        assert_eq!(parsed.text, "draft:maybe review:soon -author:x checks:");
        assert_eq!(parsed.filters.checks, Some(ChecksFilter::Passing));
    }

    #[test]
    fn unknown_qualifiers_are_scoped_labels_except_paths() {
        let parsed = parse_query("area:ui -kind:chore http://example.com/x");
        assert_eq!(parsed.filters.labels, vec![vec!["area:ui".to_string()]]);
        assert_eq!(
            parsed.filters.excluded_labels,
            vec!["kind:chore".to_string()]
        );
        assert_eq!(parsed.text, "http://example.com/x");
    }

    #[test]
    fn quoted_values_keep_their_spaces() {
        let parsed = parse_query("label:\"needs review\" \"exact phrase\"");
        assert_eq!(
            parsed.filters.labels,
            vec![vec!["needs review".to_string()]]
        );
        assert_eq!(parsed.text, "\"exact phrase\"");
    }

    #[test]
    fn typed_qualifiers_outrank_the_menu() {
        let menu = Filters {
            draft: Some(DraftFilter::Only),
            review: Some(ReviewFilter::Approved),
            ..Filters::default()
        };
        let typed = Filters {
            draft: Some(DraftFilter::Hide),
            labels: vec![vec!["bug".into()]],
            ..Filters::default()
        };
        let merged = menu.overlaid(&typed);
        assert_eq!(merged.draft, Some(DraftFilter::Hide));
        assert_eq!(merged.review, Some(ReviewFilter::Approved));
        assert_eq!(merged.labels, vec![vec!["bug".to_string()]]);
    }

    #[test]
    fn filters_narrow_rows() {
        let draft = entry_with(1, "a", "ann", |e| e.is_draft = true);
        let approved = entry_with(2, "b", "Me", |e| {
            e.review_decision = Some(ReviewDecision::Approved);
            e.checks_state = Some(ChecksState::Failing);
            e.labels = vec![label("Bug"), label("ui")];
        });
        let only = |filters: Filters| {
            [&draft, &approved]
                .into_iter()
                .filter(|e| matches_filters(e, &filters, Some("me")))
                .map(|e| e.number)
                .collect::<Vec<_>>()
        };
        assert_eq!(only(Filters::default()), vec![1, 2]);
        let draft_only = Filters {
            draft: Some(DraftFilter::Only),
            ..Filters::default()
        };
        assert_eq!(only(draft_only), vec![1]);
        let unreviewed = Filters {
            review: Some(ReviewFilter::None),
            ..Filters::default()
        };
        assert_eq!(only(unreviewed), vec![1]);
        let failing = Filters {
            checks: Some(ChecksFilter::Failing),
            ..Filters::default()
        };
        assert_eq!(only(failing), vec![2]);
        let bug_and_ui = Filters {
            labels: vec![vec!["bug".into()], vec!["ui".into(), "docs".into()]],
            ..Filters::default()
        };
        assert_eq!(only(bug_and_ui), vec![2]);
        let not_bug = Filters {
            excluded_labels: vec!["BUG".into()],
            ..Filters::default()
        };
        assert_eq!(only(not_bug), vec![1]);
        let mine = Filters {
            author: Some("@me".into()),
            ..Filters::default()
        };
        assert_eq!(only(mine), vec![2]);
    }

    #[test]
    fn search_scores_number_title_branch_author() {
        let e = entry_with(281, "Show raster images", "daniel", |e| {
            e.head_branch = "feat/raster".into()
        });
        let repo = "stippi/code-assistant";
        assert_eq!(score_match(&e, "#281", repo), 100);
        assert_eq!(score_match(&e, "28", repo), 0);
        assert_eq!(score_match(&e, "show raster images", repo), 90);
        assert_eq!(score_match(&e, "raster", repo), 80);
        assert_eq!(score_match(&e, "images show", repo), 70);
        assert_eq!(score_match(&e, "feat/", repo), 60);
        assert_eq!(score_match(&e, "dani", repo), 50);
        assert_eq!(score_match(&e, "stippi", repo), 40);
        assert_eq!(score_match(&e, "raster zebra", repo), 30);
        assert_eq!(score_match(&e, "zebra", repo), 10);
    }

    #[test]
    fn rows_group_by_the_viewers_part() {
        let entries = [
            entry(1, "others", "ann"),
            entry_with(2, "asked", "bob", |e| {
                e.review_request_logins = vec!["ME".into()]
            }),
            entry(3, "mine", "me"),
            entry_with(4, "team", "cy", |e| e.has_team_review_request = true),
        ];
        assert_eq!(
            arranged(&entries, SortMode::RecentlyUpdated, ""),
            vec![
                (GroupKey::Authored, vec![3]),
                (GroupKey::ReviewRequested, vec![2, 4]),
                (GroupKey::Others, vec![1]),
            ]
        );
    }

    #[test]
    fn without_a_viewer_everything_is_others() {
        let entries = [entry(1, "a", "me"), entry(2, "b", "ann")];
        let filters = Filters::default();
        let groups = arrange(
            &entries,
            &Arrangement {
                sort: SortMode::RecentlyUpdated,
                query: "",
                menu_filters: &filters,
                involvement: Involvement::All,
                viewer: None,
                repository: "stippi/code-assistant",
            },
        );
        assert_eq!(numbers(&groups), vec![(GroupKey::Others, vec![1, 2])]);
    }

    #[test]
    fn merge_readiness_puts_ready_first_and_conflicts_last() {
        let entries = [
            entry_with(1, "conflict", "ann", |e| {
                e.mergeability = Mergeability::Conflicting
            }),
            entry_with(2, "draft", "ann", |e| e.is_draft = true),
            entry_with(3, "passing", "ann", |e| {
                e.checks_state = Some(ChecksState::Passing)
            }),
            entry_with(4, "ready", "ann", |e| {
                e.checks_state = Some(ChecksState::Passing);
                e.review_decision = Some(ReviewDecision::Approved);
            }),
            entry_with(5, "closed", "ann", |e| e.state = PullRequestState::Closed),
        ];
        assert_eq!(
            arranged(&entries, SortMode::MergeReadiness, ""),
            vec![(GroupKey::Others, vec![4, 3, 2, 5, 1])]
        );
    }

    #[test]
    fn merge_readiness_prefers_small_measured_changes_within_a_tier() {
        let entries = [
            entry(1, "unmeasured", "ann"),
            entry_with(2, "large", "ann", |e| e.additions = 500),
            entry_with(3, "small", "ann", |e| e.deletions = 5),
        ];
        assert_eq!(
            arranged(&entries, SortMode::MergeReadiness, ""),
            vec![(GroupKey::Others, vec![3, 2, 1])]
        );
    }

    #[test]
    fn blocked_on_me_ranks_my_pull_requests_by_what_blocks_them() {
        let entries = [
            entry(1, "waiting", "me"),
            entry_with(2, "failing", "me", |e| {
                e.checks_state = Some(ChecksState::Failing)
            }),
            entry_with(3, "changes", "me", |e| {
                e.review_decision = Some(ReviewDecision::ChangesRequested)
            }),
            entry_with(4, "conflict", "me", |e| {
                e.mergeability = Mergeability::Conflicting
            }),
            entry_with(5, "others keep order", "ann", |_| {}),
            entry_with(6, "others keep order", "ann", |_| {}),
        ];
        assert_eq!(
            arranged(&entries, SortMode::BlockedOnMe, ""),
            vec![
                (GroupKey::Authored, vec![4, 3, 2, 1]),
                (GroupKey::Others, vec![5, 6]),
            ]
        );
    }

    #[test]
    fn created_and_size_sorts() {
        let entries = [
            entry_with(1, "a", "ann", |e| {
                e.created_at = hours_ago(10);
                e.additions = 10;
            }),
            entry_with(2, "b", "ann", |e| {
                e.created_at = hours_ago(30);
                e.additions = 300;
            }),
            entry_with(3, "c", "ann", |e| e.created_at = hours_ago(20)),
        ];
        let order = |sort| arranged(&entries, sort, "")[0].1.clone();
        assert_eq!(order(SortMode::Newest), vec![1, 3, 2]);
        assert_eq!(order(SortMode::Oldest), vec![2, 3, 1]);
        // Unmeasured rows go last either way.
        assert_eq!(order(SortMode::Largest), vec![2, 1, 3]);
        assert_eq!(order(SortMode::Smallest), vec![1, 2, 3]);
    }

    #[test]
    fn a_search_ranks_by_match_and_overrides_readiness() {
        let entries = [
            entry_with(1, "unrelated", "ann", |e| {
                e.checks_state = Some(ChecksState::Passing);
                e.review_decision = Some(ReviewDecision::Approved);
            }),
            entry(2, "raster images", "ann"),
            entry(3, "show raster", "ann"),
        ];
        assert_eq!(
            arranged(&entries, SortMode::MergeReadiness, "raster label:x"),
            Vec::<(GroupKey, Vec<u64>)>::new(),
            "label:x keeps nothing"
        );
        assert_eq!(
            arranged(&entries, SortMode::MergeReadiness, "raster"),
            vec![(GroupKey::Others, vec![2, 3, 1])]
        );
    }

    #[test]
    fn facets_count_authors_and_labels() {
        let entries = [
            entry_with(1, "a", "ann", |e| e.labels = vec![label("bug")]),
            entry_with(2, "b", "bob", |e| {
                e.labels = vec![label("bug"), label("ui")]
            }),
            entry(3, "c", "bob"),
        ];
        let facets = facets(&entries);
        assert_eq!(
            facets.authors,
            vec![("bob".to_string(), 2), ("ann".to_string(), 1)]
        );
        assert_eq!(facets.labels, vec![(label("bug"), 2), (label("ui"), 1)]);
    }
}
