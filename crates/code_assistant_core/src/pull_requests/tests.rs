//! The store against the scripted GitHub transport: time to live,
//! revalidation and its announcement, shared reads, invalidation by writes
//! and cold starts from disk.

use super::*;
use crate::session::event_stream::{EventPayload, EventStream, Subscription};
use crate::ui::UiEvent;
use github::fake::{FakeResponse, FakeTransport};
use github::model::{PullRequestUpdate, ReactionContent};
use github::transport::Method;
use github::{PullRequestRef, RepoRef};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const PUSHED: &str = "89abcdef0123456789abcdef0123456789abcdef";
const DIFF_PATH: &str = "repos/stippi/code-assistant/pulls/291";

fn pr() -> PullRequestRef {
    RepoRef::new("github.com", "stippi", "code-assistant").pull_request(291)
}

/// The `PullRequestCore` answer the `github` crate's tests read.
fn core(change: impl FnOnce(&mut Value)) -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github/tests/fixtures/pull_request_core.json"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut data = serde_json::from_str::<Value>(&text).unwrap()["data"].take();
    change(&mut data["repository"]["pullRequest"]);
    data
}

fn titled(title: &str) -> Value {
    core(|pr| pr["title"] = title.into())
}

fn files_viewed(state: &str) -> Value {
    json!({"repository": {"pullRequest": {"files": {
        "pageInfo": {"hasNextPage": false, "endCursor": null},
        "nodes": [{"path": "a.rs", "viewerViewedState": state}]
    }}}})
}

struct Harness {
    fake: Arc<FakeTransport>,
    store: PullRequests,
    events: Subscription,
    dir: tempfile::TempDir,
}

fn harness() -> Harness {
    harness_in(tempfile::tempdir().unwrap())
}

fn harness_in(dir: tempfile::TempDir) -> Harness {
    let fake = FakeTransport::new();
    let events = EventStream::new();
    let subscription = events.subscribe();
    let source = PullRequestSource {
        client: fake.client(),
        snapshot_dir: dir.path().join("pull-requests"),
    };
    Harness {
        store: PullRequests::new(source, events),
        fake,
        events: subscription,
        dir,
    }
}

impl Harness {
    fn requests(&self, operation: &str) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.operation.as_deref().unwrap_or(&r.path) == operation)
            .count()
    }

    async fn next_event(&mut self) -> UiEvent {
        let event = tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("no event")
            .unwrap();
        assert!(
            event.session_id.is_none(),
            "pull request events are app-scoped"
        );
        match event.payload {
            EventPayload::Ui(event) => event,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Every event published so far.
    async fn drain_events(&mut self) -> Vec<UiEvent> {
        let mut events = Vec::new();
        while let Ok(Ok(event)) = tokio::time::timeout(Duration::ZERO, self.events.recv()).await {
            if let EventPayload::Ui(event) = event.payload {
                events.push(event);
            }
        }
        events
    }
}

fn changed(part: PullRequestPart) -> impl Fn(&UiEvent) -> bool {
    move |event| {
        matches!(event, UiEvent::PullRequestChanged { pr: p, part: changed }
            if *p == pr() && *changed == part)
    }
}

#[tokio::test(start_paused = true)]
async fn a_stale_detail_is_served_while_it_revalidates_and_the_change_is_announced() {
    let mut h = harness();
    h.fake
        .on_graphql("PullRequestCore")
        .once()
        .data(titled("First"));
    h.fake.on_graphql("PullRequestCore").data(titled("Second"));

    let first = h.store.detail(pr()).await.unwrap();
    tokio::time::advance(Duration::from_secs(30)).await;
    let cached = h.store.detail(pr()).await.unwrap();
    assert_eq!(
        (first.value.title.as_str(), first.refreshing),
        ("First", false)
    );
    assert_eq!(cached, first);
    assert_eq!(h.requests("PullRequestCore"), 1);

    tokio::time::advance(Duration::from_secs(31)).await;
    let stale = h.store.detail(pr()).await.unwrap();
    assert_eq!(stale.value.title, "First");
    assert!(stale.refreshing);
    assert!(changed(PullRequestPart::Detail)(&h.next_event().await));

    let fresh = h.store.detail(pr()).await.unwrap();
    assert_eq!(fresh.value.title, "Second");
    assert!(!fresh.refreshing);
    assert_eq!(h.requests("PullRequestCore"), 2);
}

#[tokio::test(start_paused = true)]
async fn concurrent_reads_of_one_part_share_one_request() {
    let h = harness();
    h.fake.on_graphql("PullRequestCore").data(titled("Shared"));

    let (a, b) = tokio::join!(h.store.detail(pr()), h.store.detail(pr()));

    assert_eq!(a.unwrap().value.title, "Shared");
    assert_eq!(b.unwrap().value.title, "Shared");
    assert_eq!(h.requests("PullRequestCore"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_merged_pull_request_stays_fresh_for_ten_minutes() {
    let h = harness();
    h.fake.on_graphql("PullRequestCore").data(core(|pr| {
        pr["state"] = "MERGED".into();
        pr["mergedAt"] = "2026-10-30T12:00:00Z".into();
    }));

    h.store.detail(pr()).await.unwrap();
    tokio::time::advance(Duration::from_secs(9 * 60)).await;
    assert!(!h.store.detail(pr()).await.unwrap().refreshing);
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(h.store.detail(pr()).await.unwrap().refreshing);
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_keeps_the_last_good_value() {
    let mut h = harness();
    h.fake
        .on_graphql("PullRequestFilesViewed")
        .once()
        .data(files_viewed("VIEWED"));
    h.fake
        .on_graphql("PullRequestFilesViewed")
        .respond(FakeResponse::status(502));

    h.store.files_viewed(pr()).await.unwrap();
    tokio::time::advance(Duration::from_secs(16)).await;
    assert!(h.store.files_viewed(pr()).await.unwrap().refreshing);
    // Announced even so: the view stops waiting for it.
    assert!(changed(PullRequestPart::FilesViewed)(&h.next_event().await));

    let kept = h.store.files_viewed(pr()).await.unwrap();
    assert_eq!(kept.value.files.len(), 1);
    assert!(!kept.refreshing, "no refresh loop after a failure");
}

#[tokio::test(start_paused = true)]
async fn a_write_invalidates_what_it_changes_and_announces_it() {
    let mut h = harness();
    h.fake
        .on_graphql("PullRequestCore")
        .once()
        .data(titled("Before"));
    h.fake.on_graphql("PullRequestCore").data(titled("After"));
    h.fake
        .on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    h.fake.on_graphql("UpdatePullRequest").data(json!({}));

    h.store.detail(pr()).await.unwrap();
    let update = PullRequestUpdate {
        title: Some("After".into()),
        ..PullRequestUpdate::default()
    };
    h.store.update(pr(), update).await.unwrap();

    let events = h.drain_events().await;
    assert!(events.iter().any(changed(PullRequestPart::Detail)));
    assert!(events.iter().any(|event| matches!(event,
        UiEvent::PullRequestListChanged { repo } if *repo == pr().repo)));
    assert!(!events.iter().any(changed(PullRequestPart::Diff)));
    // Read anew at once, not served stale.
    let read = h.store.detail(pr()).await.unwrap();
    assert_eq!(read.value.title, "After");
    assert!(!read.refreshing);
}

#[tokio::test(start_paused = true)]
async fn a_failed_write_still_invalidates() {
    let mut h = harness();
    h.fake
        .on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    h.fake
        .on_graphql("AddReaction")
        .respond(FakeResponse::errors(json!([{"message": "nope"}])));

    let result = h
        .store
        .react(pr(), None, ReactionContent::Heart, true)
        .await;

    assert!(result.is_err());
    assert!(
        h.drain_events()
            .await
            .iter()
            .any(changed(PullRequestPart::Activity))
    );
}

#[tokio::test(start_paused = true)]
async fn ticking_files_viewed_keeps_the_diff() {
    let mut h = harness();
    h.fake
        .on_rest(Method::Get, DIFF_PATH)
        .respond(FakeResponse::text(200, "diff --git a/a.rs b/a.rs\n"));
    h.fake
        .on_graphql("PullRequestFilesViewed")
        .once()
        .data(files_viewed("UNVIEWED"));
    h.fake
        .on_graphql("PullRequestFilesViewed")
        .data(files_viewed("VIEWED"));
    h.fake
        .on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    h.fake.on_graphql("SetFileViewed").data(json!({}));

    h.store.diff(pr(), None, None).await.unwrap();
    h.store.files_viewed(pr()).await.unwrap();
    h.store
        .set_files_viewed(pr(), vec![("a.rs".into(), true)])
        .await
        .unwrap();

    let events = h.drain_events().await;
    assert_eq!(events.len(), 1);
    assert!(changed(PullRequestPart::FilesViewed)(&events[0]));
    let viewed = h.store.files_viewed(pr()).await.unwrap();
    assert_eq!(
        viewed.value.files[0].state,
        github::model::FileViewedState::Viewed
    );
    h.store.diff(pr(), None, None).await.unwrap();
    assert_eq!(h.requests(DIFF_PATH), 1);
}

#[tokio::test(start_paused = true)]
async fn a_diff_read_alongside_the_detail_serves_later_reads() {
    let h = harness();
    h.fake
        .on_graphql("PullRequestCore")
        .data(core(|pr| pr["headRefOid"] = HEAD.into()));
    h.fake
        .on_rest(Method::Get, DIFF_PATH)
        .respond(FakeResponse::text(200, "diff --git a/a.rs b/a.rs\n"));

    // Opening a pull request prefetches its diff while the detail (and so
    // the head) is still on its way.
    let (detail, diff) = tokio::join!(h.store.detail(pr()), h.store.diff(pr(), None, None));
    detail.unwrap();
    diff.unwrap();
    h.store.diff(pr(), None, None).await.unwrap();
    assert_eq!(h.requests(DIFF_PATH), 1);
}

#[tokio::test(start_paused = true)]
async fn a_push_seen_by_a_detail_revalidation_reads_the_diff_anew() {
    let mut h = harness();
    h.fake
        .on_graphql("PullRequestCore")
        .once()
        .data(core(|pr| pr["headRefOid"] = HEAD.into()));
    h.fake
        .on_graphql("PullRequestCore")
        .data(core(|pr| pr["headRefOid"] = PUSHED.into()));
    h.fake
        .on_rest(Method::Get, DIFF_PATH)
        .respond(FakeResponse::text(200, "diff --git a/a.rs b/a.rs\n"));

    h.store.detail(pr()).await.unwrap();
    tokio::time::advance(Duration::from_secs(30)).await;
    h.store.diff(pr(), None, None).await.unwrap();

    // The detail goes stale and its background read sees the push; the
    // diff, still within its own time to live, must not be served.
    tokio::time::advance(Duration::from_secs(31)).await;
    h.store.detail(pr()).await.unwrap();
    assert!(changed(PullRequestPart::Detail)(&h.next_event().await));
    h.store.diff(pr(), None, None).await.unwrap();
    assert_eq!(h.requests(DIFF_PATH), 2);
}

#[tokio::test(start_paused = true)]
async fn a_push_reads_the_diff_anew() {
    let h = harness();
    h.fake
        .on_graphql("PullRequestCore")
        .once()
        .data(core(|pr| pr["headRefOid"] = HEAD.into()));
    h.fake
        .on_graphql("PullRequestCore")
        .data(core(|pr| pr["headRefOid"] = PUSHED.into()));
    h.fake
        .on_rest(Method::Get, DIFF_PATH)
        .respond(FakeResponse::text(200, "diff --git a/a.rs b/a.rs\n"));

    h.store.detail(pr()).await.unwrap();
    h.store.diff(pr(), None, None).await.unwrap();
    h.store.diff(pr(), None, None).await.unwrap();
    assert_eq!(h.requests(DIFF_PATH), 1);

    // The detail learns of the push; the diff, still within its time to
    // live, is read again for the new head.
    h.store.refresh(&pr());
    h.store.detail(pr()).await.unwrap();
    h.store.diff(pr(), None, None).await.unwrap();
    assert_eq!(h.requests(DIFF_PATH), 2);
}

#[tokio::test]
async fn a_cold_start_shows_the_snapshot_at_once_and_revalidates() {
    let first = harness();
    first
        .fake
        .on_graphql("PullRequestCore")
        .data(titled("Saved"));
    let saved = first.store.detail(pr()).await.unwrap();
    first.store.snapshots_written().await;

    let mut second = harness_in(first.dir);
    second
        .fake
        .on_graphql("PullRequestCore")
        .data(titled("Current"));
    let shown = second.store.detail(pr()).await.unwrap();
    assert_eq!(shown.value, saved.value);
    // When GitHub answered, not when the snapshot was read.
    let age = |fetched: &Fetched<_>| fetched.fetched_at.elapsed().unwrap();
    assert!(age(&shown) >= age(&saved));
    assert!(shown.refreshing);

    assert!(changed(PullRequestPart::Detail)(&second.next_event().await));
    let current = second.store.detail(pr()).await.unwrap();
    assert_eq!(current.value.title, "Current");
}

#[tokio::test(start_paused = true)]
async fn refreshing_a_repositorys_lists_reads_them_anew_and_announces_it() {
    let mut h = harness();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github/tests/fixtures/pull_request_search.json"
    );
    let search = serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap();
    h.fake
        .on_graphql("PullRequestSearch")
        .data(search["data"].clone());
    let repo = pr().repo;
    let query = github::model::ListQuery {
        limit: 25,
        ..github::model::ListQuery::default()
    };

    h.store.list(repo.clone(), query.clone()).await.unwrap();
    h.store.list(repo.clone(), query.clone()).await.unwrap();
    assert_eq!(
        h.requests("PullRequestSearch"),
        1,
        "the second read is cached"
    );

    h.store.refresh_list(&repo);
    assert!(matches!(h.next_event().await,
        UiEvent::PullRequestListChanged { repo: changed } if changed == repo));
    let read = h.store.list(repo, query).await.unwrap();
    assert!(!read.refreshing, "read anew, not served stale");
    assert_eq!(h.requests("PullRequestSearch"), 2);
}

#[tokio::test]
async fn a_stack_action_reads_every_layer_anew_even_when_refused() {
    let mut h = harness();
    h.fake
        .on_rest(
            Method::Get,
            "repos/stippi/code-assistant/stacks?pull_request=291",
        )
        .json(
            json!([{"number": 4, "url": "u", "base": "main", "pull_requests": [
                {"number": 290, "head": {"ref": "a", "sha": "aaa"}, "state": "open"},
                {"number": 291, "head": {"ref": "b", "sha": "bbb"}, "state": "open"}
            ]}]),
        );
    // The viewer saw stack 3: it changed since.
    let expected = vec![
        github::model::StackHead {
            number: 290,
            head_sha: "aaa".into(),
        },
        github::model::StackHead {
            number: 291,
            head_sha: "bbb".into(),
        },
    ];
    let refused = h
        .store
        .stack_action(pr(), 3, expected, github::model::StackAction::Rebase)
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "The stack changed. Refresh it before trying again."
    );
    let events = h.drain_events().await;
    for number in [290, 291] {
        assert!(events.iter().any(|event| matches!(
            event,
            UiEvent::PullRequestChanged { pr, part: PullRequestPart::Stack } if pr.number == number
        )));
    }
}
