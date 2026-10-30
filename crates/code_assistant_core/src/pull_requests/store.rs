//! The pull request reads and writes behind the session service, each read
//! through its part's cache.

use super::cache::{Cache, Policy, Refreshed};
use super::snapshots::{Kind, Snapshots};
use super::{Fetched, PullRequestPart, SessionPullRequests, github_remote, own_pull_request};
use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use anyhow::Result;
use futures::FutureExt;
use github::model::{
    Action, Activity, Checks, Detail, DiffFileContents, DiffPage, DiffStat, EditableCommentKind,
    FileChange, FilesViewed, LabelCandidates, ListPage, ListQuery, PullRequestState,
    PullRequestUpdate, ReactionContent, ReviewSubmission, Reviewer, ReviewerCandidates,
    ThreadCommentsPage, Viewer,
};
use github::{Priority, PullRequestRef, RepoRef};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const LIST_TTL: Duration = Duration::from_secs(30);
const DETAIL_TTL: Duration = Duration::from_secs(60);
/// A merged or closed pull request rarely changes.
const SETTLED_DETAIL_TTL: Duration = Duration::from_secs(10 * 60);
const CHECKS_TTL: Duration = Duration::from_secs(15);
const ACTIVITY_TTL: Duration = Duration::from_secs(60);
const DIFF_TTL: Duration = Duration::from_secs(60);
/// A commit's diff never changes; the time to live only bounds memory.
const COMMIT_DIFF_TTL: Duration = Duration::from_secs(10 * 60);
const FILES_VIEWED_TTL: Duration = Duration::from_secs(15);
const VIEWER_TTL: Duration = Duration::from_secs(10 * 60);
const CANDIDATES_TTL: Duration = Duration::from_secs(10 * 60);
const HEAD_TTL: Duration = Duration::from_secs(60);

const PULL_REQUESTS_HELD: usize = 128;
const LISTS_HELD: usize = 64;
const DIFF_PAGES_HELD: usize = 128;
/// What all held diff pages may weigh together.
const DIFF_BUDGET: usize = 32 * 1024 * 1024;
const VIEWERS_HELD: usize = 32;
const CANDIDATES_HELD: usize = 32;

/// Where pull requests are read from and where their snapshots are kept.
#[derive(Clone)]
pub struct PullRequestSource {
    pub client: github::Client,
    /// Disk snapshots for a cold start.
    pub snapshot_dir: PathBuf,
}

impl Default for PullRequestSource {
    /// GitHub over HTTPS with the token from the environment or `gh`;
    /// snapshots under `<config_dir>/pull-requests/`.
    fn default() -> Self {
        Self {
            client: github::Client::new(),
            snapshot_dir: crate::config_dir::config_dir().join("pull-requests"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// A push drops the pull request's diffs when a detail read first sees the
/// new head (see [`PullRequests::detail`]); the head is not part of the key,
/// so a diff read before the detail arrived still serves later reads.
struct DiffKey {
    pr: PullRequestRef,
    commit: Option<String>,
    cursor: Option<String>,
}

type ListKey = (RepoRef, ListQuery);

/// Pull request reads and writes for every view, cached per part. One per
/// session service.
pub struct PullRequests {
    /// Background priority; reads and writes a user waits for ask for more.
    client: github::Client,
    events: EventStream,
    snapshots: Arc<Snapshots>,
    lists: Cache<ListKey, ListPage>,
    details: Cache<PullRequestRef, Detail>,
    checks: Cache<PullRequestRef, Checks>,
    activities: Cache<PullRequestRef, Activity>,
    diffs: Cache<DiffKey, DiffPage>,
    files_viewed: Cache<PullRequestRef, FilesViewed>,
    viewers: Cache<String, Viewer>,
    reviewer_candidates: Cache<PullRequestRef, ReviewerCandidates>,
    label_candidates: Cache<PullRequestRef, LabelCandidates>,
    /// The own pull request of a branch, by repository and branch name.
    heads: Cache<(RepoRef, String), Option<u64>>,
}

/// Announces a pull request's part after a background refresh.
fn announce<K: 'static>(
    events: &EventStream,
    part: PullRequestPart,
    pr: fn(&K) -> &PullRequestRef,
) -> Option<Refreshed<K>> {
    let events = events.clone();
    Some(Box::new(move |key| {
        events.publish_app(UiEvent::PullRequestChanged {
            pr: pr(key).clone(),
            part,
        })
    }))
}

fn itself(pr: &PullRequestRef) -> &PullRequestRef {
    pr
}

fn policy<K, V>(ttl: fn(&K, &V) -> Duration, capacity: usize) -> Policy<K, V> {
    Policy {
        ttl,
        capacity,
        budget: None,
    }
}

impl PullRequests {
    pub fn new(source: PullRequestSource, events: EventStream) -> Self {
        let lists_changed = {
            let events = events.clone();
            Box::new(move |(repo, _): &ListKey| {
                events.publish_app(UiEvent::PullRequestListChanged { repo: repo.clone() })
            }) as Refreshed<ListKey>
        };
        Self {
            client: source.client.with_priority(Priority::Background),
            snapshots: Arc::new(Snapshots::new(source.snapshot_dir)),
            lists: Cache::new(policy(|_, _| LIST_TTL, LISTS_HELD), Some(lists_changed)),
            details: Cache::new(
                policy(
                    |_, detail: &Detail| match detail.state {
                        PullRequestState::Open => DETAIL_TTL,
                        PullRequestState::Merged | PullRequestState::Closed => SETTLED_DETAIL_TTL,
                    },
                    PULL_REQUESTS_HELD,
                ),
                announce(&events, PullRequestPart::Detail, itself),
            ),
            checks: Cache::new(
                policy(|_, _| CHECKS_TTL, PULL_REQUESTS_HELD),
                announce(&events, PullRequestPart::Checks, itself),
            ),
            activities: Cache::new(
                policy(|_, _| ACTIVITY_TTL, PULL_REQUESTS_HELD),
                announce(&events, PullRequestPart::Activity, itself),
            ),
            diffs: Cache::new(
                Policy {
                    ttl: |key: &DiffKey, _| match key.commit {
                        Some(_) => COMMIT_DIFF_TTL,
                        None => DIFF_TTL,
                    },
                    capacity: DIFF_PAGES_HELD,
                    budget: Some((|page: &DiffPage| page.patch.len(), DIFF_BUDGET)),
                },
                announce(&events, PullRequestPart::Diff, |key: &DiffKey| &key.pr),
            ),
            files_viewed: Cache::new(
                policy(|_, _| FILES_VIEWED_TTL, PULL_REQUESTS_HELD),
                announce(&events, PullRequestPart::FilesViewed, itself),
            ),
            viewers: Cache::new(policy(|_, _| VIEWER_TTL, VIEWERS_HELD), None),
            reviewer_candidates: Cache::new(policy(|_, _| CANDIDATES_TTL, CANDIDATES_HELD), None),
            label_candidates: Cache::new(policy(|_, _| CANDIDATES_TTL, CANDIDATES_HELD), None),
            heads: Cache::new(policy(|_, _| HEAD_TTL, PULL_REQUESTS_HELD), None),
            events,
        }
    }

    /// The client at background priority, for reads nobody waits for (the
    /// lifecycle sweep).
    pub fn client(&self) -> github::Client {
        self.client.clone()
    }

    fn interactive(&self) -> github::Client {
        self.client.with_priority(Priority::Interactive)
    }

    /// The repository of the checkout at `path`, the branch the session
    /// works on (`branch`, else the one checked out unless it is the base
    /// branch) and its pull request (`known`, else looked up by branch).
    /// `None` when the checkout has no GitHub remote.
    pub async fn session_pull_requests(
        &self,
        path: PathBuf,
        branch: Option<String>,
        known: Option<u64>,
    ) -> Result<Option<SessionPullRequests>> {
        let checkout = tokio::task::spawn_blocking(move || {
            let repo = git::GitRepository::open(&path).ok()?;
            let github = github_remote(&repo)?;
            let checked_out = repo.current_branch().filter(|branch| {
                !repo
                    .default_base_branch()
                    .is_some_and(|base| crate::session::lifecycle::is_base_branch(branch, &base))
            });
            Some((github, checked_out))
        })
        .await?;
        let Some((repo, checked_out)) = checkout else {
            return Ok(None);
        };
        let head_branch = branch.or(checked_out);
        let own = match (known, &head_branch) {
            (Some(number), _) => Some(number),
            (None, Some(branch)) => {
                let key = (repo.clone(), branch.clone());
                read(
                    &self.heads,
                    key,
                    &self.client,
                    |client, (repo, branch)| async move {
                        let found = client.pull_requests_for_head(&repo, &branch).await?;
                        Ok(own_pull_request(&found))
                    },
                )
                .await?
                .value
            }
            (None, None) => None,
        };
        Ok(Some(SessionPullRequests {
            repo,
            head_branch,
            own,
        }))
    }

    pub async fn list(&self, repo: RepoRef, query: ListQuery) -> Result<Fetched<ListPage>> {
        // Only first pages are kept on disk: a cold start opens no other.
        let first_page = query.cursor.is_none();
        let key = (repo, query);
        if first_page {
            self.seed(&self.lists, Kind::List, &key).await;
        }
        let snapshots = self.snapshots.clone();
        read(
            &self.lists,
            key,
            &self.client,
            move |client, key| async move {
                let page = client.list(&key.0, &key.1).await?;
                if first_page {
                    snapshots.save(Kind::List, &key, &page);
                }
                Ok(page)
            },
        )
        .await
    }

    pub async fn list_stats(&self, prs: Vec<PullRequestRef>) -> Result<Vec<DiffStat>> {
        Ok(self.interactive().list_stats(&prs).await?)
    }

    pub async fn detail(&self, pr: PullRequestRef) -> Result<Fetched<Detail>> {
        self.seed(&self.details, Kind::Detail, &pr).await;
        let snapshots = self.snapshots.clone();
        let (details, diffs) = (self.details.clone(), self.diffs.clone());
        read(
            &self.details,
            pr,
            &self.client,
            move |client, pr| async move {
                let detail = client.detail(&pr).await?;
                let held_head = details.peek(&pr, |held| held.head_sha.clone()).flatten();
                if held_head.is_some() && held_head != detail.head_sha {
                    diffs.invalidate(|key| key.pr == pr && key.commit.is_none());
                }
                snapshots.save(Kind::Detail, &pr, &detail);
                Ok(detail)
            },
        )
        .await
    }

    pub async fn checks(&self, pr: PullRequestRef) -> Result<Fetched<Checks>> {
        read(&self.checks, pr, &self.client, |client, pr| async move {
            client.checks(&pr).await
        })
        .await
    }

    pub async fn activity(&self, pr: PullRequestRef) -> Result<Fetched<Activity>> {
        self.seed(&self.activities, Kind::Activity, &pr).await;
        let snapshots = self.snapshots.clone();
        read(
            &self.activities,
            pr,
            &self.client,
            move |client, pr| async move {
                let activity = client.activity(&pr).await?;
                snapshots.save(Kind::Activity, &pr, &activity);
                Ok(activity)
            },
        )
        .await
    }

    /// A page of the pull request's diff, or of one of its commits'.
    pub async fn diff(
        &self,
        pr: PullRequestRef,
        commit: Option<String>,
        cursor: Option<String>,
    ) -> Result<Fetched<DiffPage>> {
        let key = DiffKey { pr, commit, cursor };
        read(&self.diffs, key, &self.client, |client, key| async move {
            let cursor = key.cursor.as_deref();
            match &key.commit {
                Some(sha) => client.commit_diff(&key.pr.repo, sha, cursor).await,
                None => client.diff(&key.pr, cursor).await,
            }
        })
        .await
    }

    pub async fn files_viewed(&self, pr: PullRequestRef) -> Result<Fetched<FilesViewed>> {
        read(
            &self.files_viewed,
            pr,
            &self.client,
            |client, pr| async move { client.files_viewed(&pr).await },
        )
        .await
    }

    pub async fn thread_comments(
        &self,
        pr: PullRequestRef,
        thread_id: String,
        cursor: String,
    ) -> Result<ThreadCommentsPage> {
        Ok(self
            .interactive()
            .thread_comments(&pr, &thread_id, &cursor)
            .await?)
    }

    pub async fn file_contents(
        &self,
        pr: PullRequestRef,
        commit: Option<String>,
        change: FileChange,
        old_path: String,
        new_path: String,
    ) -> Result<DiffFileContents> {
        Ok(self
            .interactive()
            .diff_file_contents(&pr, commit.as_deref(), change, &old_path, &new_path)
            .await?)
    }

    pub async fn reviewer_candidates(&self, pr: PullRequestRef) -> Result<ReviewerCandidates> {
        let read = read(
            &self.reviewer_candidates,
            pr,
            &self.client,
            |client, pr| async move { client.reviewer_candidates(&pr).await },
        );
        Ok(read.await?.value)
    }

    pub async fn label_candidates(&self, pr: PullRequestRef) -> Result<LabelCandidates> {
        let read = read(
            &self.label_candidates,
            pr,
            &self.client,
            |client, pr| async move { client.label_candidates(&pr).await },
        );
        Ok(read.await?.value)
    }

    pub async fn viewer(&self, host: String) -> Result<Viewer> {
        let read = read(
            &self.viewers,
            host,
            &self.client,
            |client, host| async move { client.viewer(&host).await },
        );
        Ok(read.await?.value)
    }

    pub async fn comment(&self, pr: PullRequestRef, body: String) -> Result<()> {
        let written = self.interactive().add_comment(&pr, &body).await;
        self.written(&pr, &[PullRequestPart::Activity], written)
    }

    pub async fn update_comment(
        &self,
        pr: PullRequestRef,
        id: String,
        kind: EditableCommentKind,
        body: String,
    ) -> Result<()> {
        let written = self
            .interactive()
            .update_comment(&pr, &id, kind, &body)
            .await;
        self.written(&pr, &[PullRequestPart::Activity], written)
    }

    pub async fn update(&self, pr: PullRequestRef, update: PullRequestUpdate) -> Result<()> {
        let written = self.interactive().update_pull_request(&pr, &update).await;
        self.written(&pr, &[PullRequestPart::Detail], written)
    }

    pub async fn submit_review(&self, pr: PullRequestRef, review: ReviewSubmission) -> Result<()> {
        let written = self.interactive().submit_review(&pr, &review).await;
        let parts = [PullRequestPart::Detail, PullRequestPart::Activity];
        self.written(&pr, &parts, written)
    }

    pub async fn reply(&self, pr: PullRequestRef, thread_id: String, body: String) -> Result<()> {
        let written = self
            .interactive()
            .reply_to_thread(&pr, &thread_id, &body)
            .await;
        self.written(&pr, &[PullRequestPart::Activity], written)
    }

    pub async fn resolve_thread(
        &self,
        pr: PullRequestRef,
        thread_id: String,
        resolved: bool,
    ) -> Result<()> {
        let written = self
            .interactive()
            .set_thread_resolved(&pr, &thread_id, resolved)
            .await;
        self.written(&pr, &[PullRequestPart::Activity], written)
    }

    pub async fn react(
        &self,
        pr: PullRequestRef,
        subject_id: Option<String>,
        content: ReactionContent,
        reacted: bool,
    ) -> Result<()> {
        let written = self
            .interactive()
            .set_reaction(&pr, subject_id.as_deref(), content, reacted)
            .await;
        self.written(&pr, &[PullRequestPart::Activity], written)
    }

    pub async fn request_reviewers(
        &self,
        pr: PullRequestRef,
        reviewers: Vec<Reviewer>,
        requested: bool,
    ) -> Result<()> {
        let written = self
            .interactive()
            .set_reviewers_requested(&pr, &reviewers, requested)
            .await;
        self.reviewer_candidates.invalidate(|key| *key == pr);
        let parts = [PullRequestPart::Detail, PullRequestPart::Activity];
        self.written(&pr, &parts, written)
    }

    pub async fn set_labels(
        &self,
        pr: PullRequestRef,
        labels: Vec<String>,
        applied: bool,
    ) -> Result<()> {
        let written = self.interactive().set_labels(&pr, &labels, applied).await;
        self.label_candidates.invalidate(|key| *key == pr);
        self.written(&pr, &[PullRequestPart::Detail], written)
    }

    /// Ticking files off changes nothing else: the diff stays.
    pub async fn set_files_viewed(
        &self,
        pr: PullRequestRef,
        files: Vec<(String, bool)>,
    ) -> Result<()> {
        let written = self.interactive().set_files_viewed(&pr, &files).await;
        self.written(&pr, &[PullRequestPart::FilesViewed], written)
    }

    pub async fn action(&self, pr: PullRequestRef, action: Action) -> Result<()> {
        let written = self.interactive().run_action(&pr, action).await;
        let parts = [
            PullRequestPart::Detail,
            PullRequestPart::Checks,
            PullRequestPart::Activity,
        ];
        self.written(&pr, &parts, written)
    }

    /// Every part of the pull request is read anew on its next read.
    pub fn refresh(&self, pr: &PullRequestRef) {
        self.reviewer_candidates.invalidate(|key| key == pr);
        self.label_candidates.invalidate(|key| key == pr);
        let parts = [
            PullRequestPart::Detail,
            PullRequestPart::Checks,
            PullRequestPart::Activity,
            PullRequestPart::Diff,
            PullRequestPart::FilesViewed,
        ];
        self.invalidate(pr, &parts);
    }

    /// The repository's lists are read anew on their next read.
    pub fn refresh_list(&self, repo: &RepoRef) {
        self.lists.invalidate(|(listed, _)| listed == repo);
        self.events
            .publish_app(UiEvent::PullRequestListChanged { repo: repo.clone() });
    }

    /// After a write, failed or not (GitHub may have applied part of it),
    /// the parts it changes are read anew.
    fn written(
        &self,
        pr: &PullRequestRef,
        parts: &[PullRequestPart],
        written: github::Result<()>,
    ) -> Result<()> {
        self.invalidate(pr, parts);
        Ok(written?)
    }

    /// Invalidates the parts and announces each; a change to the detail
    /// also shows in the repository's lists.
    fn invalidate(&self, pr: &PullRequestRef, parts: &[PullRequestPart]) {
        for part in parts {
            match part {
                PullRequestPart::Detail => self.details.invalidate(|key| key == pr),
                PullRequestPart::Checks => self.checks.invalidate(|key| key == pr),
                PullRequestPart::Activity => self.activities.invalidate(|key| key == pr),
                PullRequestPart::Diff => self.diffs.invalidate(|key| key.pr == *pr),
                PullRequestPart::FilesViewed => self.files_viewed.invalidate(|key| key == pr),
            }
            self.events.publish_app(UiEvent::PullRequestChanged {
                pr: pr.clone(),
                part: *part,
            });
        }
        if parts.contains(&PullRequestPart::Detail) {
            self.lists.invalidate(|(repo, _)| *repo == pr.repo);
            self.events.publish_app(UiEvent::PullRequestListChanged {
                repo: pr.repo.clone(),
            });
        }
    }

    /// Holds the disk snapshot of `key` when memory holds nothing for it.
    async fn seed<K, V>(&self, cache: &Cache<K, V>, kind: Kind, key: &K)
    where
        K: Hash + Eq + Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
        V: Clone + Send + Sync + DeserializeOwned + 'static,
    {
        if cache.holds(key) {
            return;
        }
        if let Some((value, fetched_at)) = self.snapshots.load(kind, key).await {
            cache.seed(key.clone(), value, fetched_at);
        }
    }

    #[cfg(test)]
    pub(super) async fn snapshots_written(&self) {
        self.snapshots.flushed().await;
    }
}

/// Reads `key` through `cache`; `fetch` gets the client at the priority
/// the cache asks for.
async fn read<K, V, F, Fut>(
    cache: &Cache<K, V>,
    key: K,
    client: &github::Client,
    fetch: F,
) -> Result<Fetched<V>>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    F: FnOnce(github::Client, K) -> Fut,
    Fut: Future<Output = github::Result<V>> + Send + 'static,
{
    let client = client.clone();
    let fetch_key = key.clone();
    let fetched = cache
        .read(key, move |priority| {
            fetch(client.with_priority(priority), fetch_key).boxed()
        })
        .await?;
    Ok(fetched)
}
