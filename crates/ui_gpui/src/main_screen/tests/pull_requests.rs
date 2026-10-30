//! The pull request view in front of the real session core, GitHub
//! scripted: the session's repository found from its project, the list read
//! through the cache (page after page) and read anew on refresh, the session's own pull
//! request shown and commented on.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::PendingLLMProvider;
use code_assistant_core::session::SessionConfig;
use github::fake::FakeTransport;
use github::transport::Method;
use gpui_kit::TestAppContext;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed");
}

/// A repository whose `origin` is stippi/code-assistant on GitHub.
fn github_checkout(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let remote = "git@github.com:stippi/code-assistant.git";
    git(dir, &["remote", "add", "origin", remote]);
}

/// The `data` of one of the `github` crate's GraphQL fixtures.
fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../github/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let answer: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    answer["data"].clone()
}

/// GitHub as the `github` crate's fixtures have it; `own` is the pull
/// request of the checked-out branch.
fn scripted_github(own: Option<u64>) -> Arc<FakeTransport> {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSearch")
        .data(fixture("pull_request_search"));
    let heads: Vec<Value> = own
        .into_iter()
        .map(|number| {
            json!({
                "number": number, "title": "Pull request view",
                "url": format!("https://github.com/stippi/code-assistant/pull/{number}"),
                "baseRefName": "main", "headRefName": "feature/x",
                "headRefOid": "0123456789abcdef0123456789abcdef01234567",
                "state": "OPEN", "isDraft": false, "mergedAt": null, "closedAt": null,
                "updatedAt": "2026-10-30T10:00:00Z", "isCrossRepository": false,
                "headRepository": {"name": "code-assistant", "nameWithOwner": "stippi/code-assistant"},
                "headRepositoryOwner": {"login": "stippi"}
            })
        })
        .collect();
    fake.on_graphql("PullRequestsByHead")
        .data(json!({"repository": {"pullRequests": {"nodes": heads}}}));
    fake.on_graphql("PullRequestCore")
        .data(fixture("pull_request_core"));
    fake.on_rest(Method::Get, "user")
        .json(json!({"id": 1, "login": "stippi"}));
    fake
}

/// The main screen showing a session on a checkout of stippi/code-assistant
/// at branch `feature/x`.
fn session_on_checkout(
    cx: &mut TestAppContext,
    fake: &Arc<FakeTransport>,
) -> (MainScreenTest, TempDir) {
    let checkout = tempfile::tempdir().unwrap();
    github_checkout(checkout.path());
    git(checkout.path(), &["checkout", "-q", "-b", "feature/x"]);
    let core = TestCore::with_github(PendingLLMProvider::default().into_factory(), fake);
    let config = SessionConfig {
        init_path: Some(checkout.path().to_path_buf()),
        ..SessionConfig::default()
    };
    let session_id = core
        .block_on(core.service.create_session_with_config(None, config, None))
        .unwrap();
    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id.clone(), None);
    test.wait_until("the session is shown", |test| {
        test.gpui.get_current_session_id().as_deref() == Some(session_id.as_str())
    });
    (test, checkout)
}

fn searches(fake: &FakeTransport) -> usize {
    fake.operations()
        .iter()
        .filter(|op| *op == "PullRequestSearch")
        .count()
}

#[gpui_kit::test]
fn the_tab_lists_the_projects_pull_requests_and_refreshes_them(cx: &mut TestAppContext) {
    let fake = scripted_github(None);
    let (mut test, _checkout) = session_on_checkout(cx, &fake);

    test.click("right-panel-view-PR");
    // The open one: the list shows the listed state (Open) only.
    test.wait_until("the list shows", |test| test.shows("pr-row-291"));
    assert!(!test.shows("pr-row-290"), "#290 is merged");
    // The fixture says more follow: the end of the rows asks for them.
    test.wait_until("the next page was read", |_| searches(&fake) >= 2);
    let read = searches(&fake);

    test.click("pr-list-refresh");
    test.wait_until("the list was read anew", |_| searches(&fake) > read);
}

#[gpui_kit::test]
fn the_sessions_pull_request_shows_and_takes_a_comment(cx: &mut TestAppContext) {
    let fake = scripted_github(Some(291));
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("AddComment").data(json!({}));
    let (mut test, _checkout) = session_on_checkout(cx, &fake);

    test.click("right-panel-view-PR");
    test.wait_until("the pull request is read", |test| test.shows("pr-composer"));
    test.click("pr-composer");
    test.cx.simulate_input("Looks good");
    test.click("pr-comment-post");
    test.wait_until("the comment is posted", |_| {
        fake.operations().iter().any(|op| op == "AddComment")
    });
    let comment = fake
        .requests()
        .into_iter()
        .find(|r| r.operation.as_deref() == Some("AddComment"))
        .unwrap();
    assert_eq!(comment.variables.unwrap()["body"], json!("Looks good"));
    test.wait_until("the composer closes", |test| !test.shows("pr-comment-post"));
}

#[gpui_kit::test]
fn a_review_goes_with_the_pending_line_comments(cx: &mut TestAppContext) {
    use crate::main_screen::right_panel::pull_request::review_draft::{
        PendingComment, review_drafts,
    };
    use github::model::DiffSide;

    let fake = scripted_github(Some(291));
    let reviews = "repos/stippi/code-assistant/pulls/291/reviews";
    fake.on_rest(Method::Post, reviews).json(json!({"id": 1}));
    let (mut test, _checkout) = session_on_checkout(cx, &fake);
    let pr = github::RepoRef::new("github.com", "stippi", "code-assistant").pull_request(291);
    let drafts = test.cx.update(|_, cx| review_drafts(cx));
    drafts.update(&mut test.cx, |drafts, cx| {
        let comment = PendingComment {
            id: 0,
            path: "src/lib.rs".into(),
            side: DiffSide::Right,
            line: 12,
            start_side: None,
            start_line: None,
            body: "Name this".into(),
            excerpt: "+let x = 1;".into(),
        };
        drafts.add(&pr, comment, cx);
    });

    test.click("right-panel-view-PR");
    test.wait_until("the pull request is read", |test| test.shows("pr-composer"));
    // A pending line comment opens the review form.
    test.click("pr-composer");
    test.click("pr-review-submit");
    test.wait_until("the review is sent", |_| {
        fake.requests().iter().any(|r| r.path == reviews)
    });
    let review = fake
        .requests()
        .into_iter()
        .find(|r| r.path == reviews)
        .unwrap();
    let body = review.body.expect("a JSON body");
    assert_eq!(body["comments"][0]["body"], json!("Name this"));
    let pr_for_check = pr.clone();
    test.wait_until("the pending comment is gone", move |test| {
        let drafts = drafts.clone();
        let pr = pr_for_check.clone();
        test.cx.update(|_, cx| drafts.read(cx).count(&pr) == 0)
    });
}
