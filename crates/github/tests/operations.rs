//! Reads and writes, through the scripted transport.

use github::fake::{FakeResponse, FakeTransport};
use github::model::{
    Action, CheckStatus, DiffSide, EditableCommentKind, FileChange, Involvement, ListQuery,
    ListState, MergeMethod, PullRequestState, PullRequestUpdate, ReactionContent,
    ReviewCommentDraft, ReviewSubmission, ReviewVerdict, Reviewer, ReviewerKind, UpdateMethod,
};
use github::transport::Method;
use github::{BranchDeletionRefusal, Error, FileUnavailableReason, PullRequestRef, RepoRef};
use serde_json::{Value, json};

fn repo() -> RepoRef {
    RepoRef::new("github.com", "stippi", "code-assistant")
}

fn pr() -> PullRequestRef {
    repo().pull_request(291)
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str::<Value>(&text).unwrap()["data"].clone()
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

// --- Detail ------------------------------------------------------------------

#[tokio::test]
async fn reads_the_detail_with_the_variables_the_document_needs() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestCore")
        .data(fixture("pull_request_core.json"));
    let detail = fake.client().detail(&pr()).await.unwrap();
    assert_eq!(detail.number, 291);
    assert_eq!(detail.checks.len(), 3);
    let variables = fake.requests()[0].variables.clone().unwrap();
    assert_eq!(
        variables,
        json!({
            "owner": "stippi", "name": "code-assistant", "number": 291,
            "headRef": "refs/pull/291/head", "required": true, "requestedReviewers": 100
        })
    );
}

#[tokio::test]
async fn reads_the_detail_again_without_reviewers_when_a_team_is_refused() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestCore")
        .with_variables(json!({"requestedReviewers": 100}))
        .respond(FakeResponse::errors(json!([{
            "type": "FORBIDDEN",
            "path": ["repository", "pullRequest", "reviewRequests", "nodes", 2, "requestedReviewer"],
            "message": "Resource not accessible by integration"
        }])));
    fake.on_graphql("PullRequestCore")
        .with_variables(json!({"requestedReviewers": 0}))
        .data(fixture("pull_request_core.json"));
    let detail = fake.client().detail(&pr()).await.unwrap();
    assert_eq!(detail.number, 291);
    assert_eq!(fake.operations(), ["PullRequestCore", "PullRequestCore"]);
}

#[tokio::test]
async fn walks_every_check_context_past_the_first_hundred() {
    let fake = FakeTransport::new();
    let mut core = fixture("pull_request_core.json");
    core["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
        ["pageInfo"]["hasNextPage"] = true.into();
    fake.on_graphql("PullRequestCore").data(core);
    let page = |contexts: Value, next: Option<&str>| {
        json!({"repository": {"pullRequest": {
            "headRefOid": HEAD,
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {
                "nodes": contexts,
                "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}
            }}}}]}
        }}})
    };
    fake.on_graphql("PullRequestCheckContexts")
        .with_variables(json!({"after": null}))
        .data(page(
            json!([{"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]),
            Some("p2"),
        ));
    fake.on_graphql("PullRequestCheckContexts")
        .with_variables(json!({"after": "p2"}))
        .data(page(
            json!([{"name": "deploy", "status": "IN_PROGRESS"}]),
            None,
        ));
    let detail = fake.client().detail(&pr()).await.unwrap();
    let checks: Vec<_> = detail
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.status))
        .collect();
    assert_eq!(
        checks,
        [
            ("build", CheckStatus::Success),
            ("deploy", CheckStatus::Pending)
        ]
    );
    assert!(!detail.checks_truncated);
}

#[tokio::test]
async fn fails_when_the_head_moves_while_checks_are_walked() {
    let fake = FakeTransport::new();
    let mut core = fixture("pull_request_core.json");
    core["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
        ["pageInfo"]["hasNextPage"] = true.into();
    fake.on_graphql("PullRequestCore").data(core);
    fake.on_graphql("PullRequestCheckContexts")
        .data(json!({"repository": {"pullRequest": {
            "headRefOid": "ffffffffffffffffffffffffffffffffffffffff",
            "commits": {"nodes": []}
        }}}));
    let error = fake.client().detail(&pr()).await.unwrap_err();
    assert!(matches!(error, Error::Decode { .. }), "{error:?}");
}

// --- Checks ------------------------------------------------------------------

fn script_checks_endpoints(fake: &FakeTransport) {
    let root = "repos/stippi/code-assistant";
    fake.on_rest(Method::Get, &format!("{root}/pulls/291"))
        .once()
        .respond(
            FakeResponse::json(200, json!({"head": {"sha": HEAD}})).header("etag", "\"pull-1\""),
        );
    fake.on_rest(Method::Get, &format!("{root}/pulls/291"))
        .respond(FakeResponse::status(304));
    let runs = format!("{root}/commits/{HEAD}/check-runs?filter=all&per_page=100&page=1");
    fake.on_rest(Method::Get, &runs).once().respond(
        FakeResponse::json(200, json!({"total_count": 1, "check_runs": []}))
            .header("etag", "\"runs-1\""),
    );
    fake.on_rest(Method::Get, &runs)
        .once()
        .respond(FakeResponse::status(304));
    fake.on_rest(Method::Get, &runs).respond(
        FakeResponse::json(200, json!({"total_count": 2, "check_runs": []}))
            .header("etag", "\"runs-2\""),
    );
    let status = format!("{root}/commits/{HEAD}/status?per_page=100&page=1");
    fake.on_rest(Method::Get, &status).once().respond(
        FakeResponse::json(200, json!({"state": "success"})).header("etag", "\"status-1\""),
    );
    fake.on_rest(Method::Get, &status)
        .respond(FakeResponse::status(304));
}

#[tokio::test]
async fn polls_checks_with_etags_and_reads_in_full_only_when_something_changed() {
    let fake = FakeTransport::new();
    script_checks_endpoints(&fake);
    fake.on_graphql("PullRequestCore")
        .data(fixture("pull_request_core.json"));
    let client = fake.client();

    let first = client.checks(&pr()).await.unwrap();
    assert_eq!(first.state, PullRequestState::Open);
    assert_eq!(first.checks.len(), 3);
    assert_eq!(fake.operations(), ["PullRequestCore"]);

    // Nothing changed: three free 304s, no full read.
    let before = fake.requests().len();
    let second = client.checks(&pr()).await.unwrap();
    assert_eq!(second, first);
    let polled = &fake.requests()[before..];
    assert_eq!(polled.len(), 3);
    assert!(polled.iter().all(|r| r.operation.is_none()));
    assert!(
        polled
            .iter()
            .any(|r| r.headers.get("if-none-match") == Some("\"runs-1\""))
    );

    // A new check run: read in full again.
    client.checks(&pr()).await.unwrap();
    assert_eq!(fake.operations(), ["PullRequestCore", "PullRequestCore"]);
}

#[tokio::test]
async fn reads_checks_in_full_where_the_host_lacks_the_endpoints() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "repos/stippi/code-assistant/pulls/291")
        .respond(FakeResponse::status(404));
    fake.on_graphql("PullRequestCore")
        .data(fixture("pull_request_core.json"));
    let client = fake.client();
    client.checks(&pr()).await.unwrap();
    client.checks(&pr()).await.unwrap();
    let rest: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.operation.is_none())
        .collect();
    assert_eq!(rest.len(), 1, "the endpoint is not asked again");
    assert_eq!(fake.operations().len(), 2);
}

// --- Activity ----------------------------------------------------------------

#[tokio::test]
async fn reads_the_conversation_paging_comments_and_reviews_separately() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestActivity")
        .with_variables(json!({"head": true}))
        .data(fixture("pull_request_activity.json"));
    fake.on_graphql("PullRequestActivity")
        .with_variables(json!({
            "head": false, "withComments": true, "commentsAfter": "comments-2", "withReviews": false
        }))
        .data(json!({"repository": {"pullRequest": {"comments": {
            "pageInfo": {"hasNextPage": false, "endCursor": "comments-3"},
            "nodes": [{"id": "IC_3", "body": "Late", "createdAt": "2026-10-30T14:00:00Z"}]
        }}}}));
    fake.on_graphql("PullRequestCommitStats")
        .with_variables(json!({"ids": ["C_first"]}))
        .data(json!({"nodes": [{"__typename": "Commit", "oid": "1111111111111111111111111111111111111111", "additions": 5, "deletions": 2}]}));
    fake.on_graphql("ReviewThreads")
        .data(fixture("review_threads.json"));

    let activity = fake.client().activity(&pr()).await.unwrap();
    let ids: Vec<_> = activity.comments.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "IC_1",
            "PRRC_1",
            "PRR_approve",
            "PRRC_2",
            "IC_2",
            "PRRC_3",
            "PRR_dismissed",
            "IC_3"
        ]
    );
    assert_eq!(activity.commits[0].additions, Some(5));
    assert_eq!(activity.review_threads.len(), 2);
    assert!(!activity.review_threads_truncated);
    assert_eq!(activity.reactions[0].count, 14);
    let mut operations = fake.operations();
    operations.sort();
    assert_eq!(
        operations,
        [
            "PullRequestActivity",
            "PullRequestActivity",
            "PullRequestCommitStats",
            "ReviewThreads"
        ]
    );
}

#[tokio::test]
async fn keeps_the_conversation_when_the_thread_read_fails() {
    let fake = FakeTransport::new();
    let mut activity = fixture("pull_request_activity.json");
    activity["repository"]["pullRequest"]["comments"]["pageInfo"]["hasNextPage"] = false.into();
    fake.on_graphql("PullRequestActivity").data(activity);
    fake.on_graphql("PullRequestCommitStats")
        .respond(FakeResponse::status(502));
    fake.on_graphql("ReviewThreads")
        .respond(FakeResponse::status(502));
    let activity = fake.client().activity(&pr()).await.unwrap();
    assert_eq!(activity.comments.len(), 4);
    assert!(activity.review_threads.is_empty());
    assert!(activity.review_threads_truncated);
    assert_eq!(activity.commits[0].additions, None);
}

#[tokio::test]
async fn follows_review_threads_and_dismissals_past_their_first_page() {
    let fake = FakeTransport::new();
    let mut activity = fixture("pull_request_activity.json");
    activity["repository"]["pullRequest"]["comments"]["pageInfo"]["hasNextPage"] = false.into();
    fake.on_graphql("PullRequestActivity").data(activity);
    fake.on_graphql("PullRequestCommitStats")
        .data(json!({"nodes": []}));
    let mut first = fixture("review_threads.json");
    first["repository"]["pullRequest"]["reviewThreads"]["pageInfo"] =
        json!({"hasNextPage": true, "endCursor": "threads-2"});
    first["repository"]["pullRequest"]["reviewDismissals"]["pageInfo"] =
        json!({"hasNextPage": true, "endCursor": "dismissals-2"});
    fake.on_graphql("ReviewThreads")
        .with_variables(json!({"after": null}))
        .data(first);
    fake.on_graphql("ReviewThreads")
        .with_variables(json!({"after": "threads-2"}))
        .data(json!({"viewer": {"login": "me"}, "repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [{"id": "PRRT_9", "path": "README.md", "line": 1, "diffSide": "RIGHT",
                "comments": {"totalCount": 1, "nodes": [{"id": "PRRC_9", "body": "typo", "createdAt": "2026-10-30T15:00:00Z"}]}}]
        }}}}));
    fake.on_graphql("ReviewDismissals")
        .with_variables(json!({"after": "dismissals-2"}))
        .data(json!({"repository": {"pullRequest": {"timelineItems": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [{"dismissalMessage": "Stale", "review": {"id": "PRR_other"}}]
        }}}}));
    let activity = fake.client().activity(&pr()).await.unwrap();
    assert_eq!(activity.review_threads.len(), 3);
    assert_eq!(activity.review_threads[2].id, "PRRT_9");
    assert!(fake.operations().contains(&"ReviewDismissals".to_string()));
}

#[tokio::test]
async fn refuses_thread_comments_of_another_pull_request() {
    let fake = FakeTransport::new();
    fake.on_graphql("ReviewThreadComments").data(json!({
        "viewer": {"login": "me"},
        "repository": {"pullRequest": {"id": "PR_291"}},
        "node": {"__typename": "PullRequestReviewThread", "pullRequest": {"id": "PR_7"},
            "comments": {"pageInfo": {"hasNextPage": false}, "nodes": []}}
    }));
    let error = fake
        .client()
        .thread_comments(&pr(), "PRRT_x", "cursor")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::SubjectScope));
}

// --- Diffs and files ---------------------------------------------------------

fn files_page(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|i| json!({"filename": format!("f{i}.rs"), "status": "modified", "patch": "@@ -1 +1 @@\n-a\n+b"}))
            .collect(),
    )
}

#[tokio::test]
async fn reads_the_whole_diff_at_once() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "repos/stippi/code-assistant/pulls/291")
        .respond(FakeResponse::text(200, "diff --git a/x b/x\n"));
    let page = fake.client().diff(&pr(), None).await.unwrap();
    assert_eq!(page.patch, "diff --git a/x b/x\n");
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        fake.requests()[0].headers.get("accept"),
        Some("application/vnd.github.diff")
    );
}

#[tokio::test]
async fn pages_the_files_api_when_github_refuses_the_whole_diff() {
    let fake = FakeTransport::new();
    let files = "repos/stippi/code-assistant/pulls/291/files";
    fake.on_rest(Method::Get, "repos/stippi/code-assistant/pulls/291")
        .respond(FakeResponse::json(
            406,
            json!({"message": "Sorry, the diff exceeded the maximum number of files (300)."}),
        ));
    fake.on_rest(Method::Get, &format!("{files}?per_page=100&page=1"))
        .json(files_page(100));
    fake.on_rest(Method::Get, &format!("{files}?per_page=100&page=2"))
        .json(files_page(3));
    let client = fake.client();
    let first = client.diff(&pr(), None).await.unwrap();
    assert_eq!(first.patch.matches("diff --git").count(), 100);
    assert_eq!(first.next_cursor.as_deref(), Some("2"));
    let second = client
        .diff(&pr(), first.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(second.patch.matches("diff --git").count(), 3);
    assert_eq!(second.next_cursor, None);
    assert!(matches!(
        client.diff(&pr(), Some("../../x")).await.unwrap_err(),
        Error::Invalid(_)
    ));
}

#[tokio::test]
async fn reads_a_commits_patch_from_its_files() {
    let fake = FakeTransport::new();
    let sha = "abcdef1234567";
    fake.on_rest(
        Method::Get,
        &format!("repos/stippi/code-assistant/commits/{sha}?per_page=100&page=1"),
    )
    .json(json!({"sha": sha, "files": [{"filename": "a.rs", "status": "added", "patch": "@@ -0,0 +1 @@\n+x"}]}));
    let page = fake.client().commit_diff(&repo(), sha, None).await.unwrap();
    assert!(
        page.patch
            .starts_with("diff --git a/a.rs b/a.rs\nnew file mode 100644\n")
    );
    assert!(matches!(
        fake.client()
            .commit_diff(&repo(), "main", None)
            .await
            .unwrap_err(),
        Error::Invalid(_)
    ));
}

#[tokio::test]
async fn reads_both_sides_of_a_changed_file_and_refuses_binaries() {
    let fake = FakeTransport::new();
    let base = "1111111111111111111111111111111111111111";
    fake.on_rest(Method::Get, "repos/stippi/code-assistant/pulls/291")
        .json(json!({"base": {"sha": base}, "head": {"sha": HEAD}}));
    fake.on_rest(
        Method::Get,
        &format!("repos/stippi/code-assistant/contents/src/old%20name.rs?ref={base}"),
    )
    .respond(FakeResponse::text(200, "old"));
    fake.on_rest(
        Method::Get,
        &format!("repos/stippi/code-assistant/contents/src/new.rs?ref={HEAD}"),
    )
    .respond(FakeResponse::text(200, "new"));
    fake.on_rest(
        Method::Get,
        &format!("repos/stippi/code-assistant/contents/logo.png?ref={HEAD}"),
    )
    .respond(FakeResponse::text(200, "\u{0}PNG"));
    let client = fake.client();
    let contents = client
        .diff_file_contents(
            &pr(),
            None,
            FileChange::Renamed,
            "src/old name.rs",
            "src/new.rs",
        )
        .await
        .unwrap();
    assert_eq!(
        (contents.old.as_str(), contents.new.as_str()),
        ("old", "new")
    );
    let raw = fake
        .requests()
        .into_iter()
        .find(|r| r.path.contains("contents/src/new.rs"))
        .unwrap();
    assert_eq!(
        raw.headers.get("accept"),
        Some("application/vnd.github.raw+json")
    );
    let error = client
        .file_contents(&repo(), "logo.png", HEAD)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::FileUnavailable {
            reason: FileUnavailableReason::Binary,
            ..
        }
    ));
}

#[tokio::test]
async fn refuses_a_file_over_one_megabyte() {
    let fake = FakeTransport::new();
    fake.on_rest(
        Method::Get,
        &format!("repos/stippi/code-assistant/contents/big.txt?ref={HEAD}"),
    )
    .respond(FakeResponse::text(200, &"x".repeat(1024 * 1024 + 1)));
    let error = fake
        .client()
        .file_contents(&repo(), "big.txt", HEAD)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::FileUnavailable {
            reason: FileUnavailableReason::Oversized,
            ..
        }
    ));
}

#[tokio::test]
async fn marks_files_viewed_aliased_a_hundred_at_a_time_with_a_cached_node_id() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("SetFileViewed").data(json!({}));
    let client = fake.client();
    let files: Vec<(String, bool)> = (0..150).map(|i| (format!("f{i}.rs"), i % 2 == 0)).collect();
    client.set_files_viewed(&pr(), &files).await.unwrap();
    client.set_files_viewed(&pr(), &files[..1]).await.unwrap();
    assert_eq!(
        fake.operations(),
        [
            "PullRequestNodeId",
            "SetFileViewed",
            "SetFileViewed",
            "SetFileViewed"
        ]
    );
    let first = &fake.requests()[1];
    let variables = first.variables.as_ref().unwrap();
    assert_eq!(variables["path_99"], "f99.rs");
    assert_eq!(variables["viewed_1"], false);
    assert_eq!(variables["pullRequestId_0"], "PR_291");
    let query = first.query.as_deref().unwrap();
    assert!(query.contains(
        "a1_markFileAsViewed: markFileAsViewed(input: { pullRequestId: $pullRequestId_1, path: $path_1 }) @include(if: $viewed_1)"
    ));
}

// --- Lists -------------------------------------------------------------------

#[tokio::test]
async fn lists_through_search_with_an_opaque_cursor() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSearch")
        .data(fixture("pull_request_search.json"));
    let client = fake.client();
    let query = ListQuery {
        state: ListState::All,
        involvement: Involvement::Authored,
        text: None,
        limit: 2,
        cursor: None,
    };
    let page = client.list(&repo(), &query).await.unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.next_cursor.as_deref(), Some("s:Y3Vyc29yOjI="));
    let variables = fake.requests()[0].variables.clone().unwrap();
    assert_eq!(
        variables["q"],
        "is:pr author:@me sort:updated-desc repo:stippi/code-assistant"
    );
    assert_eq!(variables["first"], 2);

    let next = ListQuery {
        cursor: page.next_cursor,
        ..query
    };
    client.list(&repo(), &next).await.unwrap();
    assert_eq!(
        fake.requests()[1].variables.as_ref().unwrap()["after"],
        "Y3Vyc29yOjI="
    );
}

#[tokio::test]
async fn lists_a_repository_search_does_not_cover_without_search() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSearch")
        .data(json!({"search": {"pageInfo": {"hasNextPage": false}, "nodes": []}}));
    fake.on_rest(Method::Get, "user")
        .json(json!({"id": 1, "login": "contributor"}));
    let mut rows = fixture("pull_request_search.json")["search"].clone();
    rows["nodes"][0]["author"]["login"] = "contributor".into();
    fake.on_graphql("PullRequestList")
        .data(json!({"repository": {"pullRequests": rows}}));
    let query = ListQuery {
        state: ListState::Open,
        involvement: Involvement::Authored,
        text: None,
        limit: 1,
        cursor: None,
    };
    let page = fake.client().list(&repo(), &query).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        page.entries[0].author.as_ref().unwrap().login,
        "contributor"
    );
    assert_eq!(page.next_cursor.as_deref(), Some("l:Y3Vyc29yOjI="));
    let list = fake
        .requests()
        .into_iter()
        .find(|r| r.operation.as_deref() == Some("PullRequestList"))
        .unwrap();
    assert_eq!(list.variables.unwrap()["states"], json!(["OPEN"]));
}

#[tokio::test]
async fn finds_pull_requests_by_head_branch() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestsByHead")
        .with_variables(json!({"head": "feature/pr-view"}))
        .data(json!({"repository": {"pullRequests": {"nodes": [{
            "number": 291, "title": "Pull request view", "url": "https://github.com/stippi/code-assistant/pull/291",
            "baseRefName": "main", "headRefName": "feature/pr-view", "headRefOid": HEAD,
            "state": "MERGED", "isDraft": false, "mergedAt": "2026-10-30T10:00:00Z", "closedAt": "2026-10-30T10:00:00Z",
            "updatedAt": "2026-10-30T10:00:00Z", "isCrossRepository": false,
            "headRepository": {"name": "code-assistant", "nameWithOwner": "stippi/code-assistant"},
            "headRepositoryOwner": {"login": "stippi"}
        }]}}}));
    let found = fake
        .client()
        .pull_requests_for_head(&repo(), "feature/pr-view")
        .await
        .unwrap();
    assert_eq!(found[0].number, 291);
    assert_eq!(found[0].state, PullRequestState::Merged);
    assert_eq!(found[0].head_sha.as_deref(), Some(HEAD));
}

// --- Writes ------------------------------------------------------------------

#[tokio::test]
async fn submits_a_review_in_one_rest_request() {
    let fake = FakeTransport::new();
    fake.on_rest(
        Method::Post,
        "repos/stippi/code-assistant/pulls/291/reviews",
    )
    .json(json!({"id": 1}));
    fake.client()
        .submit_review(
            &pr(),
            &ReviewSubmission {
                verdict: ReviewVerdict::Approve,
                body: "LGTM".into(),
                comments: vec![ReviewCommentDraft {
                    path: "src/lib.rs".into(),
                    line: 10,
                    side: DiffSide::Right,
                    start_line: Some(10),
                    start_side: Some(DiffSide::Right),
                    body: "nice".into(),
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[0].body,
        Some(json!({
            "event": "APPROVE",
            "body": "LGTM",
            "comments": [{"path": "src/lib.rs", "line": 10, "side": "RIGHT", "body": "nice"}]
        }))
    );
}

#[tokio::test]
async fn edits_a_comment_only_after_checking_it_belongs_to_the_pull_request() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSubject")
        .with_variables(json!({"subjectId": "IC_mine"}))
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}},
            "node": {"__typename": "IssueComment", "id": "IC_mine", "pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("PullRequestSubject")
        .with_variables(json!({"subjectId": "PRRC_other"}))
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}},
            "node": {"__typename": "PullRequestReviewComment", "id": "PRRC_other", "reviewCommentPullRequest": {"id": "PR_7"}}}));
    fake.on_graphql("UpdateIssueComment")
        .data(json!({"updateIssueComment": {"issueComment": {"id": "IC_mine"}}}));
    let client = fake.client();
    client
        .update_comment(&pr(), "IC_mine", EditableCommentKind::IssueComment, "fixed")
        .await
        .unwrap();
    let error = client
        .update_comment(&pr(), "PRRC_other", EditableCommentKind::ReviewComment, "x")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::SubjectScope));
    assert_eq!(
        fake.operations(),
        [
            "PullRequestSubject",
            "UpdateIssueComment",
            "PullRequestSubject"
        ]
    );
}

#[tokio::test]
async fn reacts_to_the_pull_request_itself_by_its_node_id() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("RemoveReaction").data(json!({}));
    fake.client()
        .set_reaction(&pr(), None, ReactionContent::ThumbsDown, false)
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[1].variables,
        Some(json!({"subjectId": "PR_291", "content": "THUMBS_DOWN"}))
    );
}

#[tokio::test]
async fn updates_only_the_fields_given() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("UpdatePullRequest").data(json!({}));
    fake.client()
        .update_pull_request(
            &pr(),
            &PullRequestUpdate {
                title: Some("New title".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[1].variables,
        Some(json!({"pullRequestId": "PR_291", "title": "New title"}))
    );
}

#[tokio::test]
async fn requests_reviewers_and_removes_labels_over_rest() {
    let fake = FakeTransport::new();
    fake.on_rest(
        Method::Delete,
        "repos/stippi/code-assistant/pulls/291/requested_reviewers",
    )
    .json(json!({}));
    fake.on_rest(
        Method::Delete,
        "repos/stippi/code-assistant/issues/291/labels/needs%20review",
    )
    .json(json!([]));
    let client = fake.client();
    client
        .set_reviewers_requested(
            &pr(),
            &[Reviewer {
                id: "core".into(),
                kind: ReviewerKind::Team,
            }],
            false,
        )
        .await
        .unwrap();
    client
        .set_labels(&pr(), &["needs review".into()], false)
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[0].body,
        Some(json!({"reviewers": [], "team_reviewers": ["core"]}))
    );
}

// --- Actions -----------------------------------------------------------------

fn action_state(queue: bool, status: &str, behind_by: u64) -> Value {
    json!({"repository": {"pullRequest": {
        "id": "PR_291", "headRefOid": HEAD, "isMergeQueueEnabled": queue,
        "mergeStateStatus": status, "baseRef": {"compare": {"behindBy": behind_by}}
    }}})
}

#[tokio::test]
async fn arms_auto_merge_while_requirements_are_pending() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestActionState")
        .data(action_state(false, "BLOCKED", 0));
    fake.on_graphql("EnablePullRequestAutoMerge")
        .data(json!({}));
    fake.client()
        .run_action(&pr(), Action::EnableAutoMerge(MergeMethod::Squash))
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[1].variables,
        Some(json!({"input": {"pullRequestId": "PR_291", "mergeMethod": "SQUASH"}}))
    );
}

#[tokio::test]
async fn merges_at_once_when_nothing_is_pending_and_through_a_merge_queue_otherwise() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestActionState")
        .once()
        .data(action_state(false, "CLEAN", 0));
    fake.on_graphql("PullRequestActionState")
        .data(action_state(true, "CLEAN", 0));
    fake.on_graphql("MergePullRequest").data(json!({}));
    fake.on_graphql("EnablePullRequestAutoMerge")
        .data(json!({}));
    let client = fake.client();
    client
        .run_action(&pr(), Action::EnableAutoMerge(MergeMethod::Merge))
        .await
        .unwrap();
    client
        .run_action(&pr(), Action::Merge(MergeMethod::Rebase))
        .await
        .unwrap();
    assert_eq!(
        fake.operations(),
        [
            "PullRequestActionState",
            "MergePullRequest",
            "PullRequestActionState",
            "EnablePullRequestAutoMerge"
        ]
    );
}

#[tokio::test]
async fn updates_a_branch_only_when_behind_and_against_the_head_it_saw() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestActionState")
        .once()
        .data(action_state(false, "BEHIND", 0));
    fake.on_graphql("PullRequestActionState")
        .data(action_state(false, "BEHIND", 2));
    fake.on_graphql("UpdatePullRequestBranch").data(json!({}));
    let client = fake.client();
    client
        .run_action(&pr(), Action::UpdateBranch(UpdateMethod::Rebase))
        .await
        .unwrap();
    client
        .run_action(&pr(), Action::UpdateBranch(UpdateMethod::Rebase))
        .await
        .unwrap();
    assert_eq!(
        fake.operations(),
        [
            "PullRequestActionState",
            "PullRequestActionState",
            "UpdatePullRequestBranch"
        ]
    );
    assert_eq!(
        fake.requests()[2].variables,
        Some(json!({"pullRequestId": "PR_291", "expectedHeadOid": HEAD, "updateMethod": "REBASE"}))
    );
}

#[tokio::test]
async fn closes_through_the_pull_requests_node_id() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
    fake.on_graphql("ClosePullRequest").data(json!({}));
    fake.client()
        .run_action(&pr(), Action::Close)
        .await
        .unwrap();
    assert_eq!(
        fake.requests()[1].variables,
        Some(json!({"pullRequestId": "PR_291"}))
    );
}

fn branch_to_delete(state: &str, head: &str, base: &str) -> Value {
    json!({"repository": {"pullRequest": {
        "state": state, "headRefName": head, "baseRefName": base, "headRefOid": HEAD,
        "headRepository": {"id": "R_fork"}
    }}})
}

fn source_branch(default: &str, oid: Option<&str>) -> Value {
    json!({"node": {
        "__typename": "Repository",
        "defaultBranchRef": {"name": default},
        "ref": oid.map(|oid| json!({"target": {"oid": oid}}))
    }})
}

#[tokio::test]
async fn deletes_a_merged_branch_only_while_it_points_at_the_head() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestBranchToDelete")
        .data(branch_to_delete("MERGED", "feature/pr-view", "main"));
    fake.on_graphql("PullRequestSourceBranch")
        .once()
        .data(source_branch("main", Some(HEAD)));
    fake.on_graphql("PullRequestSourceBranch")
        .once()
        .data(source_branch(
            "main",
            Some("ffffffffffffffffffffffffffffffffffffffff"),
        ));
    fake.on_graphql("PullRequestSourceBranch")
        .data(source_branch("main", None));
    fake.on_graphql("DeletePullRequestBranch").data(json!({}));
    let client = fake.client();

    client
        .run_action(&pr(), Action::DeleteBranch)
        .await
        .unwrap();
    let delete = fake
        .requests()
        .into_iter()
        .find(|r| r.operation.as_deref() == Some("DeletePullRequestBranch"))
        .unwrap();
    assert_eq!(
        delete.variables,
        Some(json!({"input": {"repositoryId": "R_fork", "refUpdates": [{
            "name": "refs/heads/feature/pr-view",
            "beforeOid": HEAD,
            "afterOid": "0000000000000000000000000000000000000000"
        }]}}))
    );

    let moved = client
        .run_action(&pr(), Action::DeleteBranch)
        .await
        .unwrap_err();
    assert!(matches!(
        moved,
        Error::BranchDeletionRefused(BranchDeletionRefusal::HeadChanged)
    ));

    // Already gone: nothing to do.
    client
        .run_action(&pr(), Action::DeleteBranch)
        .await
        .unwrap();
    let deletes = fake
        .operations()
        .iter()
        .filter(|o| *o == "DeletePullRequestBranch")
        .count();
    assert_eq!(deletes, 1);
}

#[tokio::test]
async fn refuses_to_delete_the_branch_of_an_open_pull_request_or_a_default_branch() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestBranchToDelete")
        .once()
        .data(branch_to_delete("OPEN", "feature/pr-view", "main"));
    fake.on_graphql("PullRequestBranchToDelete")
        .data(branch_to_delete("CLOSED", "develop", "main"));
    fake.on_graphql("PullRequestSourceBranch")
        .data(source_branch("develop", Some(HEAD)));
    let client = fake.client();
    let open = client
        .run_action(&pr(), Action::DeleteBranch)
        .await
        .unwrap_err();
    assert!(matches!(
        open,
        Error::BranchDeletionRefused(BranchDeletionRefusal::OpenPullRequest)
    ));
    let default = client
        .run_action(&pr(), Action::DeleteBranch)
        .await
        .unwrap_err();
    assert!(matches!(
        default,
        Error::BranchDeletionRefused(BranchDeletionRefusal::UnsafeBranch)
    ));
    assert!(
        !fake
            .operations()
            .contains(&"DeletePullRequestBranch".to_string())
    );
}
