//! The request pipeline, through the scripted transport: auth, headers,
//! rate limits, ETags and aliased batches.

use async_trait::async_trait;
use github::fake::{FakeResponse, FakeTransport};
use github::model::ReactionContent;
use github::transport::Method;
use github::{Client, Error, Priority, PullRequestRef, RepoRef, TokenSource};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo() -> RepoRef {
    RepoRef::new("github.com", "stippi", "code-assistant")
}

fn pr(number: u64) -> PullRequestRef {
    repo().pull_request(number)
}

fn viewer_json() -> Value {
    json!({ "id": 7, "login": "stippi" })
}

struct CountingTokens(AtomicUsize);

#[async_trait]
impl TokenSource for CountingTokens {
    async fn token(&self, _host: &str) -> github::Result<String> {
        Ok(format!(
            "token-{}",
            self.0.fetch_add(1, Ordering::SeqCst) + 1
        ))
    }
}

#[tokio::test]
async fn sends_bearer_token_version_and_user_agent_to_the_dotcom_api() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user").json(viewer_json());
    let viewer = fake.client().viewer("github.com").await.unwrap();
    assert_eq!(viewer.login, "stippi");
    let request = &fake.requests()[0];
    assert_eq!(request.url, "https://api.github.com/user");
    assert_eq!(
        request.headers.get("authorization"),
        Some("Bearer test-token")
    );
    assert_eq!(
        request.headers.get("x-github-api-version"),
        Some("2022-11-28")
    );
    assert_eq!(request.headers.get("user-agent"), Some("code-assistant"));
    assert_eq!(
        request.headers.get("accept"),
        Some("application/vnd.github+json")
    );
}

#[tokio::test]
async fn talks_to_an_enterprise_server_below_api() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user").json(viewer_json());
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_1"}}}));
    fake.on_graphql("AddComment")
        .data(json!({"addComment": {"clientMutationId": null}}));
    let client = fake.client();
    client.viewer("ghe.corp").await.unwrap();
    let ghe = RepoRef::new("ghe.corp", "team", "tool").pull_request(3);
    client.add_comment(&ghe, "hello").await.unwrap();
    let urls: Vec<_> = fake.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        urls,
        [
            "https://ghe.corp/api/v3/user",
            "https://ghe.corp/api/graphql",
            "https://ghe.corp/api/graphql"
        ]
    );
}

#[tokio::test]
async fn refuses_a_host_that_is_not_a_bare_host_name() {
    let fake = FakeTransport::new();
    let error = fake.client().viewer("evil.com/path").await.unwrap_err();
    assert!(matches!(error, Error::Invalid(_)));
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn a_refused_token_is_dropped_and_the_request_retried_once() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user")
        .once()
        .respond(FakeResponse::status(401));
    fake.on_rest(Method::Get, "user").json(viewer_json());
    let client = Client::with_parts(fake.clone(), Arc::new(CountingTokens(AtomicUsize::new(0))));
    client.viewer("github.com").await.unwrap();
    let tokens: Vec<_> = fake
        .requests()
        .iter()
        .map(|r| r.headers.get("authorization").unwrap().to_string())
        .collect();
    assert_eq!(tokens, ["Bearer token-1", "Bearer token-2"]);
}

#[tokio::test]
async fn a_second_refusal_is_reported() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user")
        .respond(FakeResponse::status(401));
    let client = Client::with_parts(fake.clone(), Arc::new(CountingTokens(AtomicUsize::new(0))));
    let error = client.viewer("github.com").await.unwrap_err();
    assert!(matches!(error, Error::Unauthorized { .. }), "{error:?}");
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn reads_the_viewer_through_graphql_when_rest_refuses() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user")
        .respond(FakeResponse::json(
            403,
            json!({"message": "Resource not accessible by integration"}),
        ));
    fake.on_graphql("RoutingIdentityViewer")
        .data(json!({"viewer": {"databaseId": 9, "login": "app-user"}}));
    let viewer = fake.client().viewer("github.com").await.unwrap();
    assert_eq!((viewer.id, viewer.login.as_str()), (9, "app-user"));
}

#[tokio::test]
async fn a_graphql_rate_limit_on_http_200_pauses_background_requests() {
    let fake = FakeTransport::new();
    fake.on_graphql("ReviewerCandidates").once().respond(
        FakeResponse::errors(
            json!([{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]),
        )
        .header("retry-after", "60"),
    );
    fake.on_graphql("PullRequestNodeId")
        .data(json!({"repository": {"pullRequest": {"id": "PR_1"}}}));
    fake.on_graphql("AddReaction")
        .data(json!({"addReaction": {"reaction": {"content": "HEART"}}}));
    let client = fake.client();
    let error = client.reviewer_candidates(&pr(1)).await.unwrap_err();
    assert!(
        matches!(
            error,
            Error::RateLimited {
                retry_at: Some(_),
                ..
            }
        ),
        "{error:?}"
    );

    // Background reads stop without asking; interactive ones pass the pause.
    let paused = client.list(&repo(), &Default::default()).await.unwrap_err();
    assert!(matches!(paused, Error::Paused { .. }), "{paused:?}");
    assert_eq!(fake.requests().len(), 1);
    client
        .with_priority(Priority::Interactive)
        .set_reaction(&pr(1), None, ReactionContent::Heart, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_secondary_limit_403_counts_as_rate_limited() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Get, "user")
        .respond(FakeResponse::json(
            403,
            json!({"message": "You have exceeded a secondary rate limit."}),
        ));
    let error = fake.client().viewer("github.com").await.unwrap_err();
    assert!(error.is_rate_limited(), "{error:?}");
}

#[tokio::test]
async fn keeps_the_last_tenth_of_the_quota_for_interactive_requests() {
    let fake = FakeTransport::new();
    let reset = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600)
        .to_string();
    fake.on_rest(Method::Get, "user").respond(
        FakeResponse::json(200, viewer_json())
            .header("x-ratelimit-resource", "core")
            .header("x-ratelimit-limit", "5000")
            .header("x-ratelimit-remaining", "100")
            .header("x-ratelimit-reset", &reset),
    );
    fake.on_rest(Method::Get, "repos/stippi/code-assistant/pulls/1")
        .json(json!({"head": {"sha": "abc"}}));
    let client = fake.client();
    // `viewer` may spend the reserve.
    client.viewer("github.com").await.unwrap();
    let error = client.checks(&pr(1)).await.unwrap_err();
    assert!(matches!(error, Error::Paused { .. }), "{error:?}");
    // GraphQL has its own quota.
    fake.on_graphql("PullRequestFilesViewed")
        .data(json!({"repository": {"pullRequest": {
            "files": {"pageInfo": {"hasNextPage": false}, "nodes": []}
        }}}));
    client.files_viewed(&pr(1)).await.unwrap();
}

#[tokio::test]
async fn maps_graphql_errors_to_not_found_or_refusals() {
    let fake = FakeTransport::new();
    fake.on_graphql("LabelCandidates").respond(FakeResponse::errors(json!([
        {"type": "NOT_FOUND", "path": ["repository"], "message": "Could not resolve to a Repository"}
    ])));
    fake.on_graphql("ReviewerCandidates").respond(FakeResponse::errors(json!([
        {"type": "FORBIDDEN", "path": ["repository", "assignableUsers"], "message": "Resource not accessible"}
    ])));
    let client = fake.client();
    assert!(
        client
            .label_candidates(&pr(1))
            .await
            .unwrap_err()
            .is_not_found()
    );
    let error = client.reviewer_candidates(&pr(1)).await.unwrap_err();
    assert!(
        matches!(&error, Error::GraphQl { errors, .. } if errors[0].kind.as_deref() == Some("FORBIDDEN")),
        "{error:?}"
    );
    assert!(error.to_string().contains("Resource not accessible"));
}

#[tokio::test]
async fn reports_rest_refusals_with_githubs_messages() {
    let fake = FakeTransport::new();
    fake.on_rest(Method::Post, "repos/stippi/code-assistant/issues/1/labels").respond(
        FakeResponse::json(
            422,
            json!({"message": "Validation Failed", "errors": [{"field": "labels", "code": "invalid"}]}),
        ),
    );
    let error = fake
        .client()
        .set_labels(&pr(1), &["x".into()], true)
        .await
        .unwrap_err();
    match error {
        Error::Api {
            status, messages, ..
        } => {
            assert_eq!(status, 422);
            assert_eq!(messages, ["Validation Failed", "labels invalid"]);
        }
        other => panic!("{other:?}"),
    }
}

fn summary_node(number: u64) -> Value {
    json!({
        "number": number,
        "title": format!("PR {number}"),
        "url": format!("https://github.com/stippi/code-assistant/pull/{number}"),
        "state": "OPEN",
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "reviewDecision": null,
        "additions": 1,
        "deletions": 2,
        "changedFiles": 3,
        "updatedAt": "2026-10-30T10:00:00Z",
        "mergedAt": null,
        "closedAt": null,
        "headRefName": format!("branch-{number}"),
        "baseRefName": "main",
        "author": {"__typename": "User", "login": "stippi"},
        "latestReviews": {"nodes": []},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
    })
}

#[tokio::test]
async fn batches_summaries_25_per_request_and_isolates_a_missing_one() {
    let fake = FakeTransport::new();
    let mut first = serde_json::Map::new();
    for i in 0..25u64 {
        first.insert(
            format!("a{i}_repository"),
            json!({"pullRequest": summary_node(i + 1)}),
        );
    }
    fake.on_graphql("PullRequestSummary")
        .with_variables(json!({"number_0": 1}))
        .data(Value::Object(first));
    fake.on_graphql("PullRequestSummary")
        .with_variables(json!({"number_0": 26}))
        .respond(FakeResponse::json(
            200,
            json!({
                "data": {
                    "a0_repository": {"pullRequest": summary_node(26)},
                    "a1_repository": {"pullRequest": null}
                },
                "errors": [{
                    "type": "NOT_FOUND",
                    "path": ["a1_repository", "pullRequest"],
                    "message": "Could not resolve to a PullRequest with the number of 27."
                }]
            }),
        ));
    let prs: Vec<_> = (1..=27).map(pr).collect();
    let summaries = fake.client().summaries(&prs).await;
    assert_eq!(summaries.len(), 27);
    assert_eq!(summaries[0].as_ref().unwrap().number, 1);
    assert_eq!(summaries[25].as_ref().unwrap().head_branch, "branch-26");
    assert!(summaries[26].as_ref().unwrap_err().is_not_found());

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    let query = requests[1].query.as_deref().unwrap();
    assert!(query.starts_with(
        "query PullRequestSummary($owner_0: String!, $name_0: String!, $number_0: Int!, $owner_1:"
    ));
    assert!(query.contains("a1_repository: repository(owner: $owner_1, name: $name_1)"));
    assert_eq!(
        requests[1].variables.as_ref().unwrap()["number_1"],
        json!(27)
    );
}

#[tokio::test]
async fn an_unaddressable_reference_fails_alone_without_a_request() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSummary")
        .data(json!({"a0_repository": {"pullRequest": summary_node(1)}}));
    let bad = RepoRef::new("github.com", "stippi", "code assistant").pull_request(2);
    let summaries = fake.client().summaries(&[pr(1), bad]).await;
    assert!(summaries[0].is_ok());
    assert!(matches!(summaries[1], Err(Error::Invalid(_))));
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn a_failed_batch_fails_each_of_its_items() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestSummary")
        .respond(FakeResponse::errors(
            json!([{"message": "Something went wrong"}]),
        ));
    let summaries = fake.client().summaries(&[pr(1), pr(2)]).await;
    assert!(
        summaries
            .iter()
            .all(|s| matches!(s, Err(Error::GraphQl { .. })))
    );
}

#[tokio::test]
async fn reads_line_counts_leaving_out_missing_pull_requests() {
    let fake = FakeTransport::new();
    fake.on_graphql("PullRequestStats").respond(FakeResponse::json(
        200,
        json!({
            "data": {
                "a0_repository": {"pullRequest": {"additions": 10, "deletions": 4}},
                "a1_repository": {"pullRequest": null}
            },
            "errors": [{"type": "NOT_FOUND", "path": ["a1_repository", "pullRequest"], "message": "gone"}]
        }),
    ));
    let stats = fake.client().list_stats(&[pr(1), pr(2)]).await.unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(
        (stats[0].pr.number, stats[0].additions, stats[0].deletions),
        (1, 10, 4)
    );
}

#[tokio::test]
async fn a_truncated_answer_fails_rather_than_decoding_a_prefix() {
    let fake = FakeTransport::new();
    let big = "x".repeat(9 * 1024 * 1024);
    fake.on_graphql("LabelCandidates")
        .data(json!({"repository": {"labels": {"nodes": [{"name": big}]}}}));
    let error = fake.client().label_candidates(&pr(1)).await.unwrap_err();
    assert!(matches!(error, Error::Api { status: 200, .. }), "{error:?}");
}
