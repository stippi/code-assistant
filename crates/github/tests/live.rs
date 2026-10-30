//! Read-only checks against the real GitHub API, with the token `gh` (or
//! `GH_TOKEN`) supplies. Run by hand:
//! `cargo test -p github --test live -- --ignored --nocapture`

use github::model::{ListQuery, ListState};
use github::{Client, Priority, RepoRef};

fn repo() -> RepoRef {
    RepoRef::from_remote_url("https://github.com/stippi/code-assistant.git").unwrap()
}

#[tokio::test]
#[ignore = "talks to api.github.com"]
async fn reads_a_public_repository() {
    let client = Client::new().with_priority(Priority::Interactive);
    let repo = repo();

    let viewer = client.viewer(&repo.host).await.unwrap();
    println!("viewer: {}", viewer.login);

    let page = client
        .list(
            &repo,
            &ListQuery {
                state: ListState::All,
                limit: 5,
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(!page.entries.is_empty());
    println!(
        "listed: {:?}",
        page.entries.iter().map(|e| e.number).collect::<Vec<_>>()
    );

    let pr = repo.pull_request(page.entries[0].number);
    let older = repo.pull_request(290);
    let summaries = client.summaries(&[pr.clone(), older.clone()]).await;
    assert!(summaries.iter().all(Result::is_ok), "{summaries:?}");
    let stats = client
        .list_stats(&[pr.clone(), older.clone()])
        .await
        .unwrap();
    assert_eq!(stats.len(), 2);

    let detail = client.detail(&older).await.unwrap();
    println!(
        "#{} {:?} checks={} reviewers={:?} permissions={:?}",
        detail.number,
        detail.state,
        detail.checks.len(),
        detail.reviewers,
        detail.viewer_permissions.actions
    );
    let checks = client.checks(&older).await.unwrap();
    assert_eq!(checks.head_sha, detail.head_sha);
    // The second poll revalidates with ETags.
    client.checks(&older).await.unwrap();

    let activity = client.activity(&older).await.unwrap();
    println!(
        "comments={} threads={} commits={}",
        activity.comments.len(),
        activity.review_threads.len(),
        activity.commits.len()
    );
    assert!(!activity.commits.is_empty());

    let diff = client.diff(&older, None).await.unwrap();
    assert!(diff.patch.starts_with("diff --git"));
    let files = client.diff(&older, Some("1")).await.unwrap();
    assert!(files.patch.starts_with("diff --git"));
    let sha = &activity.commits[0].oid;
    let commit = client.commit_diff(&repo, sha, None).await.unwrap();
    assert!(commit.patch.contains("diff --git"));
    let readme = client
        .file_contents(&repo, "README.md", detail.head_sha.as_deref().unwrap())
        .await
        .unwrap();
    assert!(!readme.is_empty());

    client.files_viewed(&older).await.unwrap();
    let labels = client.label_candidates(&older).await.unwrap();
    println!("labels: {}", labels.candidates.len());
    let reviewers = client.reviewer_candidates(&older).await.unwrap();
    println!("reviewer candidates: {}", reviewers.candidates.len());
    let heads = client
        .pull_requests_for_head(&repo, &detail.head_branch)
        .await
        .unwrap();
    assert!(heads.iter().any(|h| h.number == 290));
}
