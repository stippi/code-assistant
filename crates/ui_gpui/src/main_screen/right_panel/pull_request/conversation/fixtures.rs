//! Conversation values for tests: comments, reviews, threads and commits.

use crate::main_screen::right_panel::pull_request::data::PullRequestData;
use crate::main_screen::right_panel::pull_request::fixtures::{actor, detail, repo};
use chrono::{DateTime, Utc};
use github::model::{
    Actor, Comment, CommentKind, Commit, DiffSide, ReviewState, ReviewThread, ThreadComment, Viewer,
};
use gpui_kit::{AppContext, Context, Entity, Render, TestAppContext, VisualTestContext};
use std::sync::Arc;

pub fn bot(login: &str) -> Actor {
    Actor {
        is_bot: true,
        ..actor(login)
    }
}

/// An issue comment by `login`, its body "`id` by `login`".
pub fn comment(id: &str, login: &str, at: DateTime<Utc>) -> Comment {
    Comment {
        id: id.into(),
        kind: CommentKind::IssueComment,
        author: Some(actor(login)),
        body: format!("{id} by {login}"),
        created_at: at,
        edited_at: None,
        url: Some(format!("https://github.com/c/{id}")),
        path: None,
        review_state: None,
        reactions: Vec::new(),
    }
}

pub fn review(id: &str, login: &str, state: ReviewState, at: DateTime<Utc>) -> Comment {
    Comment {
        kind: CommentKind::Review,
        review_state: Some(state),
        ..comment(id, login, at)
    }
}

/// A thread comment by "rev".
pub fn thread_reply(id: &str, at: DateTime<Utc>) -> ThreadComment {
    ThreadComment {
        id: id.into(),
        author: Some(actor("rev")),
        body: format!("{id} in a thread"),
        created_at: at,
        edited_at: None,
        url: Some(format!("https://github.com/c/{id}")),
        reactions: Vec::new(),
    }
}

/// An open thread on `src/lib.rs:12` holding `comments`, all read.
pub fn thread(id: &str, comments: &[(&str, DateTime<Utc>)]) -> ReviewThread {
    ReviewThread {
        id: id.into(),
        path: "src/lib.rs".into(),
        line: Some(12),
        side: DiffSide::Right,
        is_resolved: false,
        is_outdated: false,
        comments: comments
            .iter()
            .map(|(id, at)| thread_reply(id, *at))
            .collect(),
        comment_count: comments.len() as u64,
        next_comments_cursor: None,
    }
}

pub fn commit(oid: &str, at: DateTime<Utc>) -> Commit {
    Commit {
        oid: oid.into(),
        message_headline: format!("Commit {oid}"),
        committed_date: at,
        additions: Some(10),
        deletions: Some(2),
        authors: vec![actor("ann")],
    }
}

/// What a page knows, with `viewer` signed in; no detail.
pub fn data(cx: &mut TestAppContext, viewer: Option<&str>) -> Entity<PullRequestData> {
    let viewer = viewer.map(|login| Viewer {
        id: 1,
        login: login.into(),
    });
    cx.update(|cx| {
        cx.new(|cx| {
            let mut data = PullRequestData::new(repo().pull_request(290), cx);
            data.viewer = viewer;
            data
        })
    })
}

/// [`data`] with the detail of an open pull request the viewer may do
/// everything with.
pub fn data_with_detail(cx: &mut TestAppContext, viewer: Option<&str>) -> Entity<PullRequestData> {
    let data = data(cx, viewer);
    data.update(cx, |data, _| data.detail = Some(Arc::new(detail(290))));
    data
}

/// A window showing the view `build` makes.
pub fn window<V: Render + 'static>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Context<V>) -> V + 'static,
) -> (Entity<V>, &mut VisualTestContext) {
    let window = cx.update(|cx| {
        gpui_kit::component::init(cx);
        cx.open_window(Default::default(), |_, cx| cx.new(build))
            .unwrap()
    });
    let view = window.root(cx).unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.run_until_parked();
    (view, cx)
}
