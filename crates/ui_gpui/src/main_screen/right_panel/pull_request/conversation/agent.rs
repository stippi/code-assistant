//! "Add to agent": a comment, a review thread or a failing check handed to
//! the session's agent as a text attachment of the composer's draft (T3's
//! "Fix in a thread"). The text names what it is, where (pull request,
//! `path:line`), who said it, and links it.

use super::super::presentation::check_status_label;
use super::logic::{ReviewOutcome, ThreadItem, visible_body};
use super::parts::Location;
use crate::comments::{CommentChange, report};
use code_assistant_core::persistence::DraftAttachment;
use github::PullRequestRef;
use github::model::{Check, Comment, CommentKind};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::{App, SharedString};

/// Put `text` into the composer's draft as an attachment.
pub fn add_to_agent(text: String, cx: &mut App) {
    report(
        CommentChange::Attach(DraftAttachment::Text { content: text }),
        cx,
    );
}

/// "Add to agent", putting `text` into the draft.
pub fn add_to_agent_button(id: SharedString, text: String) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(Icon::default().path("icons/message_circle_plus.svg"))
        .label("Add to agent")
        .tooltip("Hand this to the session's agent")
        .on_click(move |_, _, cx| add_to_agent(text.clone(), cx))
}

/// `owner/repo#N`.
fn pr_label(pr: &PullRequestRef) -> String {
    format!("{}#{}", pr.repo.full_name(), pr.number)
}

fn login(author: Option<&github::model::Actor>) -> String {
    format!("@{}", author.map_or("ghost", |a| a.login.as_str()))
}

/// "on owner/repo#N, path:line (outdated)".
fn place(pr: &PullRequestRef, location: Option<&Location>, notes: &[&str]) -> String {
    let mut place = format!("on {}", pr_label(pr));
    if let Some(location) = location {
        place.push_str(&format!(", {}", location.label()));
    }
    let notes: Vec<&str> = location
        .filter(|l| l.outdated)
        .map(|_| "outdated")
        .into_iter()
        .chain(notes.iter().copied())
        .collect();
    if !notes.is_empty() {
        place.push_str(&format!(" ({})", notes.join(", ")));
    }
    place
}

/// The heading, the link under it, then the body after a blank line.
fn compose(heading: String, url: Option<&str>, body: Option<&str>) -> String {
    let mut text = heading;
    if let Some(url) = url {
        text.push('\n');
        text.push_str(url);
    }
    if let Some(body) = body {
        text.push_str("\n\n");
        text.push_str(body);
    }
    text
}

/// A comment, its kind, author and place.
pub fn comment_text(pr: &PullRequestRef, comment: &Comment, location: Option<&Location>) -> String {
    let kind = match comment.kind {
        CommentKind::IssueComment => "Comment",
        CommentKind::ReviewComment => "Review comment",
        CommentKind::Review => "Review",
    };
    let mut heading = format!(
        "{kind} by {} {}",
        login(comment.author.as_ref()),
        place(pr, location, &[])
    );
    if let Some(outcome) = ReviewOutcome::of(comment.review_state) {
        heading.push_str(&format!(": {}", outcome.label()));
    }
    compose(heading, comment.url.as_deref(), visible_body(&comment.body))
}

/// A review thread with every comment read.
pub fn thread_text(pr: &PullRequestRef, item: &ThreadItem) -> String {
    let thread = &item.thread;
    let location = Location {
        path: thread.path.clone(),
        line: thread.line,
        outdated: thread.is_outdated,
    };
    let resolved: &[&str] = if thread.is_resolved {
        &["resolved"]
    } else {
        &[]
    };
    let heading = format!("Review thread {}", place(pr, Some(&location), resolved));
    let comments: Vec<String> = std::iter::once(&item.root)
        .chain(&item.replies)
        .filter_map(|c| {
            visible_body(&c.body).map(|body| format!("{}:\n{body}", login(c.author.as_ref())))
        })
        .collect();
    let body = comments.join("\n\n");
    compose(
        heading,
        item.url.as_deref(),
        (!body.is_empty()).then_some(body.as_str()),
    )
}

/// A check that did not pass.
pub fn check_text(pr: &PullRequestRef, check: &Check) -> String {
    let mut heading = format!(
        "Check \"{}\" on {}: {}",
        check.name,
        pr_label(pr),
        check_status_label(check)
    );
    if let Some(description) = check
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        heading.push_str(&format!(" ({})", description.trim()));
    }
    compose(heading, check.url.as_deref(), None)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{comment, review, thread};
    use super::super::logic::thread_item;
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{check, hours_ago, repo};
    use github::model::{CheckStatus, ReviewState};

    fn pr() -> PullRequestRef {
        repo().pull_request(290)
    }

    #[test]
    fn a_comment_says_who_where_and_what() {
        let mut remark = comment("c1", "ann", hours_ago(1));
        remark.kind = CommentKind::ReviewComment;
        remark.body = "<!-- bot -->\nThis unwrap panics.".into();
        let location = Location {
            path: "src/lib.rs".into(),
            line: Some(12),
            outdated: true,
        };
        assert_eq!(
            comment_text(&pr(), &remark, Some(&location)),
            "Review comment by @ann on stippi/code-assistant#290, src/lib.rs:12 (outdated)\n\
             https://github.com/c/c1\n\n\
             <!-- bot -->\nThis unwrap panics."
        );
        assert_eq!(
            comment_text(&pr(), &comment("c2", "bob", hours_ago(1)), None),
            "Comment by @bob on stippi/code-assistant#290\nhttps://github.com/c/c2\n\nc2 by bob"
        );
    }

    #[test]
    fn a_review_names_its_verdict() {
        let mut verdict = review("r1", "cid", ReviewState::ChangesRequested, hours_ago(1));
        verdict.url = None;
        verdict.body = "  ".into();
        assert_eq!(
            comment_text(&pr(), &verdict, None),
            "Review by @cid on stippi/code-assistant#290: Changes requested"
        );
    }

    #[test]
    fn a_thread_quotes_every_comment() {
        let mut t = thread("t1", &[("a", hours_ago(3)), ("b", hours_ago(2))]);
        t.is_resolved = true;
        let item = thread_item(&t, &Default::default()).unwrap();
        assert_eq!(
            thread_text(&pr(), &item),
            "Review thread on stippi/code-assistant#290, src/lib.rs:12 (resolved)\n\
             https://github.com/c/a\n\n\
             @rev:\na in a thread\n\n\
             @rev:\nb in a thread"
        );
    }

    #[test]
    fn a_check_names_its_state_and_link() {
        let mut failed = check("build / test", CheckStatus::Failure);
        failed.url = Some("https://github.com/o/r/actions/runs/1".into());
        failed.description = Some("2 tests failed".into());
        assert_eq!(
            check_text(&pr(), &failed),
            "Check \"build / test\" on stippi/code-assistant#290: Failed (2 tests failed)\n\
             https://github.com/o/r/actions/runs/1"
        );
    }
}
