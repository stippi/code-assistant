//! GitHub pull requests through the service: where a session's pull
//! requests live, cached reads and the writes (see
//! [`crate::pull_requests`]). Everything runs on its own, outside the
//! session lanes: a slow host never holds up session commands.

use super::*;
use crate::pull_requests::{
    CheckoutPlace, Fetched, PullRequestCheckout, SessionCheckouts, SessionPullRequests,
    github_checkouts, pull_request_head,
};
use crate::session::pull_request::PullRequestSnapshot;
use github::model::{
    Action, Activity, Checks, Detail, DiffFileContents, DiffPage, DiffStat, EditableCommentKind,
    FileChange, FilesViewed, LabelCandidates, ListPage, ListQuery, PullRequestUpdate,
    ReactionContent, ReviewSubmission, Reviewer, ReviewerCandidates, Stack, StackAction, StackHead,
    ThreadCommentsPage, Viewer,
};
use github::{PullRequestRef, RepoRef};
use std::time::SystemTime;

impl SessionService {
    /// The GitHub repository of the session's project, its branch and that
    /// branch's pull request. `None` when the project has no GitHub remote
    /// (or the session no project).
    pub async fn session_pull_requests(
        &self,
        session_id: String,
    ) -> Result<Option<SessionPullRequests>> {
        self.call_io(move |ctx| async move {
            let (path, branch, known) = {
                let manager = ctx.manager.lock().await;
                let session = manager
                    .get_session(&session_id)
                    .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
                let Some(path) = session.session.config.effective_project_path().cloned() else {
                    return Ok(None);
                };
                let branch = session.session.config.branch.clone();
                let known = manager
                    .session_lifecycles()?
                    .remove(&session_id)
                    .and_then(|lifecycle| lifecycle.pull_request)
                    .map(|pr| pr.number);
                (path, branch, known)
            };
            ctx.pull_requests
                .session_pull_requests(path, branch, known)
                .await
        })
        .await
    }

    /// The GitHub repositories of the session's directory: its own and those
    /// checked out directly below it. Only local git is read. `None` for a
    /// session without a project.
    pub async fn session_checkouts(&self, session_id: String) -> Result<Option<SessionCheckouts>> {
        self.call_io(move |ctx| async move {
            let directory = {
                let manager = ctx.manager.lock().await;
                let session = manager
                    .get_session(&session_id)
                    .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
                session.session.config.effective_project_path().cloned()
            };
            let Some(directory) = directory else {
                return Ok(None);
            };
            let found = directory.clone();
            let checkouts = tokio::task::spawn_blocking(move || github_checkouts(&found)).await?;
            Ok(Some(SessionCheckouts {
                directory,
                checkouts,
            }))
        })
        .await
    }

    /// Check `pr` out as `gh pr checkout` does, in the checkout of its
    /// repository: `Here` in the session's directory (its branch switches)
    /// or the checkout below it (the session stays as it is), or in a
    /// worktree of its own with a new session for the project on it. The
    /// session working on it knows it as its pull request.
    pub async fn pull_request_checkout(
        &self,
        session_id: String,
        pr: PullRequestRef,
        place: CheckoutPlace,
    ) -> Result<PullRequestCheckout> {
        self.call_io(move |ctx| async move {
            let (project_root, here, project, worktree) = {
                let manager = ctx.manager.lock().await;
                let session = manager
                    .get_session(&session_id)
                    .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
                let config = &session.session.config;
                (
                    session_project_root(&manager, &session_id)?,
                    session_effective_path(&manager, &session_id)?,
                    config.initial_project.clone(),
                    config.worktree_path.clone(),
                )
            };
            let detail = ctx.pull_requests.detail(pr.clone()).await?.value;
            let directory = match place {
                CheckoutPlace::Here => here,
                // Worktrees hang off the repository itself.
                CheckoutPlace::Worktree => project_root,
            };
            let path = {
                let (directory, wanted) = (directory.clone(), pr.repo.clone());
                tokio::task::spawn_blocking(move || {
                    github_checkouts(&directory)
                        .into_iter()
                        .find(|checkout| checkout.repo == wanted)
                        .map(|checkout| checkout.path)
                })
                .await?
            }
            .ok_or_else(|| anyhow!("{} is not checked out in this project", pr.repo.full_name()))?;
            let repo = git::GitRepository::open(&path).context("Failed to open git repository")?;
            let remotes: Vec<(String, String)> = repo
                .remotes()
                .into_iter()
                .map(|remote| (remote.name, remote.url))
                .collect();
            let head = pull_request_head(&detail, &pr.repo, &remotes);
            let done = match place {
                CheckoutPlace::Here => git::pull_request::checkout_here(&repo, &head).await,
                CheckoutPlace::Worktree => {
                    git::pull_request::checkout_in_worktree(&repo, &head).await
                }
            }
            .with_context(|| format!("Could not check out pull request #{}", pr.number))?;

            let mut manager = ctx.manager.lock().await;
            let session_id = match place {
                // A session spanning several repositories has no one branch.
                CheckoutPlace::Here if path != directory => {
                    return Ok(PullRequestCheckout {
                        session_id,
                        path: done.path,
                        branch: done.branch,
                        on_head: done.on_head,
                    });
                }
                CheckoutPlace::Here => {
                    manager.set_session_worktree(
                        &session_id,
                        worktree,
                        Some(done.branch.clone()),
                    )?;
                    session_id
                }
                CheckoutPlace::Worktree => {
                    let mut config = manager.new_session_config(Some(path));
                    config.initial_project = project;
                    let model = SessionModelConfig::new(manager.default_model_name());
                    let name = format!("#{} {}", detail.number, detail.title);
                    let id = manager.create_session_with_config(
                        Some(name),
                        Some(config),
                        Some(model),
                    )?;
                    manager.set_session_worktree(
                        &id,
                        Some(done.path.clone()),
                        Some(done.branch.clone()),
                    )?;
                    id
                }
            };
            let snapshot = PullRequestSnapshot::from_detail(&detail, SystemTime::now());
            manager.update_session_lifecycle(&session_id, |lifecycle| {
                lifecycle.pull_request = Some(snapshot)
            })?;
            Ok(PullRequestCheckout {
                session_id,
                path: done.path,
                branch: done.branch,
                on_head: done.on_head,
            })
        })
        .await
    }

    // Reads: the held value at once (memory, else disk snapshot) and a
    // background refresh when it is past its time to live; with nothing
    // held, they wait for GitHub.

    pub async fn pull_request_list(
        &self,
        repo: RepoRef,
        query: ListQuery,
    ) -> Result<Fetched<ListPage>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.list(repo, query).await })
            .await
    }

    pub async fn pull_request_list_stats(&self, prs: Vec<PullRequestRef>) -> Result<Vec<DiffStat>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.list_stats(prs).await })
            .await
    }

    pub async fn pull_request_detail(&self, pr: PullRequestRef) -> Result<Fetched<Detail>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.detail(pr).await })
            .await
    }

    pub async fn pull_request_checks(&self, pr: PullRequestRef) -> Result<Fetched<Checks>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.checks(pr).await })
            .await
    }

    pub async fn pull_request_activity(&self, pr: PullRequestRef) -> Result<Fetched<Activity>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.activity(pr).await })
            .await
    }

    /// A page of the diff: the pull request's, or with `commit` that
    /// commit's. `cursor` is the previous page's `next_cursor`.
    pub async fn pull_request_diff(
        &self,
        pr: PullRequestRef,
        commit: Option<String>,
        cursor: Option<String>,
    ) -> Result<Fetched<DiffPage>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.diff(pr, commit, cursor).await })
            .await
    }

    pub async fn pull_request_stack(&self, pr: PullRequestRef) -> Result<Fetched<Option<Stack>>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.stack(pr).await })
            .await
    }

    /// Merge the stack through `pr`, or rebase the stack it tops, unless
    /// GitHub's stack differs from what `expected` saw.
    pub async fn pull_request_stack_action(
        &self,
        pr: PullRequestRef,
        stack_number: u64,
        expected: Vec<StackHead>,
        action: StackAction,
    ) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .stack_action(pr, stack_number, expected, action)
                .await
        })
        .await
    }

    pub async fn pull_request_files_viewed(
        &self,
        pr: PullRequestRef,
    ) -> Result<Fetched<FilesViewed>> {
        self.call_io(move |ctx| async move { ctx.pull_requests.files_viewed(pr).await })
            .await
    }

    /// More comments of a review thread, after `cursor` (the thread's
    /// `next_comments_cursor`, then the page's `next_cursor`).
    pub async fn pull_request_thread_comments(
        &self,
        pr: PullRequestRef,
        thread_id: String,
        cursor: String,
    ) -> Result<ThreadCommentsPage> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .thread_comments(pr, thread_id, cursor)
                .await
        })
        .await
    }

    /// Both sides of a changed file, of the pull request or of `commit`.
    pub async fn pull_request_file_contents(
        &self,
        pr: PullRequestRef,
        commit: Option<String>,
        change: FileChange,
        old_path: String,
        new_path: String,
    ) -> Result<DiffFileContents> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .file_contents(pr, commit, change, old_path, new_path)
                .await
        })
        .await
    }

    pub async fn pull_request_reviewer_candidates(
        &self,
        pr: PullRequestRef,
    ) -> Result<ReviewerCandidates> {
        self.call_io(move |ctx| async move { ctx.pull_requests.reviewer_candidates(pr).await })
            .await
    }

    pub async fn pull_request_label_candidates(
        &self,
        pr: PullRequestRef,
    ) -> Result<LabelCandidates> {
        self.call_io(move |ctx| async move { ctx.pull_requests.label_candidates(pr).await })
            .await
    }

    /// The account the host's credential signs in as.
    pub async fn github_viewer(&self, host: String) -> Result<Viewer> {
        self.call_io(move |ctx| async move { ctx.pull_requests.viewer(host).await })
            .await
    }

    // Writes: run at once, then invalidate what they change and announce it
    // with `PullRequestChanged` (and `PullRequestListChanged`).

    pub async fn pull_request_comment(&self, pr: PullRequestRef, body: String) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.comment(pr, body).await })
            .await
    }

    pub async fn pull_request_update_comment(
        &self,
        pr: PullRequestRef,
        id: String,
        kind: EditableCommentKind,
        body: String,
    ) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests.update_comment(pr, id, kind, body).await
        })
        .await
    }

    pub async fn pull_request_update(
        &self,
        pr: PullRequestRef,
        update: PullRequestUpdate,
    ) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.update(pr, update).await })
            .await
    }

    pub async fn pull_request_submit_review(
        &self,
        pr: PullRequestRef,
        review: ReviewSubmission,
    ) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.submit_review(pr, review).await })
            .await
    }

    pub async fn pull_request_reply(
        &self,
        pr: PullRequestRef,
        thread_id: String,
        body: String,
    ) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.reply(pr, thread_id, body).await })
            .await
    }

    pub async fn pull_request_resolve_thread(
        &self,
        pr: PullRequestRef,
        thread_id: String,
        resolved: bool,
    ) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .resolve_thread(pr, thread_id, resolved)
                .await
        })
        .await
    }

    /// Adds or removes the viewer's reaction on a comment or review
    /// (`subject_id`), or on the pull request itself (`None`).
    pub async fn pull_request_react(
        &self,
        pr: PullRequestRef,
        subject_id: Option<String>,
        content: ReactionContent,
        reacted: bool,
    ) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .react(pr, subject_id, content, reacted)
                .await
        })
        .await
    }

    pub async fn pull_request_request_reviewers(
        &self,
        pr: PullRequestRef,
        reviewers: Vec<Reviewer>,
        requested: bool,
    ) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests
                .request_reviewers(pr, reviewers, requested)
                .await
        })
        .await
    }

    pub async fn pull_request_set_labels(
        &self,
        pr: PullRequestRef,
        labels: Vec<String>,
        applied: bool,
    ) -> Result<()> {
        self.call_io(
            move |ctx| async move { ctx.pull_requests.set_labels(pr, labels, applied).await },
        )
        .await
    }

    /// Marks files viewed (`true`) or not; only the viewed states are read
    /// anew, the diff stays.
    pub async fn pull_request_set_files_viewed(
        &self,
        pr: PullRequestRef,
        files: Vec<(String, bool)>,
    ) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.set_files_viewed(pr, files).await })
            .await
    }

    pub async fn pull_request_action(&self, pr: PullRequestRef, action: Action) -> Result<()> {
        self.call_io(move |ctx| async move { ctx.pull_requests.action(pr, action).await })
            .await
    }

    /// Drops the repository's cached lists; each is read anew on its next
    /// read.
    pub async fn pull_request_list_refresh(&self, repo: RepoRef) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests.refresh_list(&repo);
            Ok(())
        })
        .await
    }

    /// Drops every cached part of the pull request; each is read anew on its
    /// next read.
    pub async fn pull_request_refresh(&self, pr: PullRequestRef) -> Result<()> {
        self.call_io(move |ctx| async move {
            ctx.pull_requests.refresh(&pr);
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_service_with_pull_requests;
    use super::*;
    use crate::pull_requests::PullRequestSource;
    use crate::session::pull_request::tests::head_nodes;
    use crate::session::pull_request::{PullRequestSnapshot, PullRequestState};
    use github::fake::FakeTransport;
    use serde_json::json;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn a_sessions_pull_request_comes_from_its_lifecycle_or_its_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["checkout", "-q", "-b", "feature/x"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:stippi/code-assistant.git",
            ],
        );
        let fake = FakeTransport::new();
        fake.on_graphql("PullRequestsByHead")
            .with_variables(json!({"head": "feature/x"}))
            .data(head_nodes("feature/x", &[291]));
        let source = PullRequestSource {
            client: fake.client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, manager) =
            test_service_with_pull_requests(&tmp.path().join("sessions"), source);
        let config = crate::session::SessionConfig {
            init_path: Some(repo.clone()),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        service.load_session(id.clone(), None).await.unwrap();

        // The branch checked out, looked up on GitHub.
        let found = service.session_pull_requests(id.clone()).await.unwrap();
        let expected = SessionPullRequests {
            repo: RepoRef::new("github.com", "stippi", "code-assistant"),
            head_branch: Some("feature/x".into()),
            own: Some(291),
        };
        assert_eq!(found, Some(expected.clone()));

        // A pull request the lifecycle knows needs no lookup.
        manager
            .lock()
            .await
            .update_session_lifecycle(&id, |lifecycle| {
                lifecycle.pull_request = Some(PullRequestSnapshot {
                    number: 17,
                    url: String::new(),
                    title: String::new(),
                    state: PullRequestState::Open,
                    review: None,
                    checks: None,
                    fetched_at: std::time::SystemTime::UNIX_EPOCH,
                })
            })
            .unwrap();
        let known = service.session_pull_requests(id).await.unwrap();
        assert_eq!(known.and_then(|found| found.own), Some(17));
        assert_eq!(fake.operations(), ["PullRequestsByHead"]);

        // A session without a project has no pull requests.
        let bare = service.create_session(None, None).await.unwrap();
        service.load_session(bare.clone(), None).await.unwrap();
        assert_eq!(service.session_pull_requests(bare).await.unwrap(), None);
    }

    /// A repository at `dir` with a commit and `url` as its `origin`.
    fn checkout_with_origin(dir: &std::path::Path, url: &str) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t.t"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(dir, &["remote", "add", "origin", url]);
    }

    #[tokio::test]
    async fn a_project_folder_offers_the_checkouts_below_it() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        checkout_with_origin(&project.join("alpha"), "git@github.com:team/alpha.git");
        checkout_with_origin(&project.join("beta"), "git@github.com:team/beta.git");
        git(
            &project.join("beta"),
            &["checkout", "-q", "-b", "feature/x"],
        );
        let source = PullRequestSource {
            client: FakeTransport::new().client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, _manager) =
            test_service_with_pull_requests(&tmp.path().join("sessions"), source);
        let config = crate::session::SessionConfig {
            init_path: Some(project.clone()),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        service.load_session(id.clone(), None).await.unwrap();

        let found = service
            .session_checkouts(id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.directory, project);
        let repos: Vec<_> = found
            .checkouts
            .iter()
            .map(|c| (c.repo.name.as_str(), c.branch.as_deref()))
            .collect();
        assert_eq!(repos, [("alpha", None), ("beta", Some("feature/x"))]);
        // The folder itself is no repository, as far as pull requests go.
        assert_eq!(service.session_pull_requests(id).await.unwrap(), None);

        let bare = service.create_session(None, None).await.unwrap();
        service.load_session(bare.clone(), None).await.unwrap();
        assert_eq!(service.session_checkouts(bare).await.unwrap(), None);
    }

    /// `git` in `dir`, its output.
    fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A clone at `project` of `stippi/code-assistant` that fetches over a
    /// stand-in for ssh from a local "GitHub": main, and a fork's head at
    /// refs/pull/291/head, which is returned.
    fn clone_of_github(tmp: &std::path::Path, project: &std::path::Path) -> String {
        let upstream = tmp.join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        git_out(&upstream, &["init", "-q", "-b", "main"]);
        git_out(&upstream, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git_out(
            &upstream,
            &["commit", "-q", "--allow-empty", "-m", "fork work"],
        );
        let head = git_out(&upstream, &["rev-parse", "HEAD"]);
        git_out(&upstream, &["update-ref", "refs/pull/291/head", &head]);
        git_out(&upstream, &["reset", "-q", "--hard", "HEAD~1"]);
        git_out(
            tmp,
            &[
                "clone",
                "-q",
                upstream.to_str().unwrap(),
                project.to_str().unwrap(),
            ],
        );
        let ssh = tmp.join("ssh.sh");
        std::fs::write(
            &ssh,
            format!("#!/bin/sh\nexec git upload-pack '{}'\n", upstream.display()),
        )
        .unwrap();
        git_out(
            project,
            &[
                "config",
                "core.sshCommand",
                &format!("sh {}", ssh.display()),
            ],
        );
        git_out(
            project,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:stippi/code-assistant.git",
            ],
        );
        head
    }

    /// GitHub's answer for that fork's pull request; maintainers may push.
    fn fork_pull_request(head: &str) -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../github/tests/fixtures/pull_request_core.json"
        );
        let text = std::fs::read_to_string(path).unwrap();
        let mut data = serde_json::from_str::<serde_json::Value>(&text).unwrap()["data"].take();
        data["repository"]["pullRequest"]["headRefOid"] = head.into();
        data
    }

    #[tokio::test]
    async fn a_pull_request_of_a_checkout_below_is_checked_out_there() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let tool = folder.join("tool");
        let head = clone_of_github(tmp.path(), &tool);
        let fake = FakeTransport::new();
        fake.on_graphql("PullRequestCore")
            .data(fork_pull_request(&head));
        let source = PullRequestSource {
            client: fake.client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, manager) =
            test_service_with_pull_requests(&tmp.path().join("sessions"), source);
        let config = crate::session::SessionConfig {
            init_path: Some(folder.clone()),
            initial_project: "folder".into(),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        service.load_session(id.clone(), None).await.unwrap();
        let pr = RepoRef::new("github.com", "stippi", "code-assistant").pull_request(291);

        // In a worktree: a session of its own, of the same project.
        let done = service
            .pull_request_checkout(id.clone(), pr.clone(), CheckoutPlace::Worktree)
            .await
            .unwrap();
        assert_ne!(done.session_id, id);
        assert_eq!(git_out(&done.path, &["rev-parse", "HEAD"]), head);
        {
            let manager = manager.lock().await;
            let config = &manager
                .get_session(&done.session_id)
                .unwrap()
                .session
                .config;
            assert_eq!(config.init_path.as_ref(), Some(&tool));
            assert_eq!(config.worktree_path.as_ref(), Some(&done.path));
            assert_eq!(config.initial_project, "folder");
        }
        let in_worktree = service
            .session_checkouts(done.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            in_worktree.checkouts.len(),
            1,
            "a session on one repository"
        );

        // Here: the checkout below moves, the session spans them still.
        git_out(&done.path, &["checkout", "-q", "--detach"]);
        let done = service
            .pull_request_checkout(id.clone(), pr.clone(), CheckoutPlace::Here)
            .await
            .unwrap();
        assert_eq!(done.session_id, id);
        assert_eq!(done.path, tool);
        assert_eq!(git_out(&tool, &["rev-parse", "HEAD"]), head);
        let manager = manager.lock().await;
        let config = &manager.get_session(&id).unwrap().session.config;
        assert_eq!(config.branch, None);
        assert_eq!(config.worktree_path, None);
        let lifecycle = manager.session_lifecycles().unwrap().remove(&id);
        assert!(lifecycle.is_none_or(|lifecycle| lifecycle.pull_request.is_none()));

        // A repository not checked out here says so.
        drop(manager);
        let other = RepoRef::new("github.com", "stippi", "other").pull_request(291);
        let refused = service
            .pull_request_checkout(id, other, CheckoutPlace::Here)
            .await
            .unwrap_err();
        assert!(format!("{refused:#}").contains("stippi/other is not checked out"));
    }

    #[tokio::test]
    async fn a_forks_pull_request_is_checked_out_in_a_worktree_with_a_session_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("code-assistant");
        let head = clone_of_github(tmp.path(), &project);

        let fake = FakeTransport::new();
        fake.on_graphql("PullRequestCore")
            .data(fork_pull_request(&head));
        let source = PullRequestSource {
            client: fake.client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, manager) =
            test_service_with_pull_requests(&tmp.path().join("sessions"), source);
        let config = crate::session::SessionConfig {
            init_path: Some(project.clone()),
            initial_project: "code-assistant".into(),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        let pr = RepoRef::new("github.com", "stippi", "code-assistant").pull_request(291);

        let done = service
            .pull_request_checkout(id.clone(), pr, CheckoutPlace::Worktree)
            .await
            .unwrap();
        assert_ne!(done.session_id, id);
        assert_eq!(done.branch, "feature/pr-view");
        assert!(done.on_head);
        assert_eq!(git_out(&done.path, &["rev-parse", "HEAD"]), head);
        // The project itself stays where it was.
        assert_eq!(git_out(&project, &["branch", "--show-current"]), "main");
        // Pushed back to the fork, like `gh pr checkout`.
        assert_eq!(
            git_out(&project, &["config", "branch.feature/pr-view.remote"]),
            "git@github.com:contributor/code-assistant.git"
        );

        let manager = manager.lock().await;
        let session = manager.get_session(&done.session_id).unwrap();
        assert_eq!(
            session.session.config.worktree_path.as_ref(),
            Some(&done.path)
        );
        assert_eq!(
            session.session.config.branch.as_deref(),
            Some("feature/pr-view")
        );
        assert_eq!(session.session.config.initial_project, "code-assistant");
        let lifecycle = manager.session_lifecycles().unwrap();
        assert_eq!(
            lifecycle[&done.session_id]
                .pull_request
                .as_ref()
                .map(|pr| pr.number),
            Some(291)
        );
    }

    #[tokio::test]
    async fn reads_and_writes_reach_github_through_the_service() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeTransport::new();
        fake.on_rest(
            github::transport::Method::Get,
            "repos/stippi/code-assistant/pulls/291",
        )
        .respond(github::fake::FakeResponse::text(
            200,
            "diff --git a/a b/a\n",
        ));
        fake.on_graphql("PullRequestNodeId")
            .data(json!({"repository": {"pullRequest": {"id": "PR_291"}}}));
        fake.on_graphql("AddComment").data(json!({}));
        let source = PullRequestSource {
            client: fake.client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, _manager) =
            test_service_with_pull_requests(&tmp.path().join("sessions"), source);
        let pr = RepoRef::new("github.com", "stippi", "code-assistant").pull_request(291);
        let mut subscription = service.subscribe();

        let diff = service
            .pull_request_diff(pr.clone(), None, None)
            .await
            .unwrap();
        service
            .pull_request_comment(pr.clone(), "Looks good".into())
            .await
            .unwrap();

        assert_eq!(diff.value.patch, "diff --git a/a b/a\n");
        let event = subscription.recv().await.unwrap();
        assert!(event.session_id.is_none());
        assert!(matches!(
            event.payload,
            crate::session::EventPayload::Ui(UiEvent::PullRequestChanged {
                pr: changed,
                part: crate::pull_requests::PullRequestPart::Activity,
            }) if changed == pr
        ));
        let comment = fake.requests().into_iter().last().unwrap();
        assert_eq!(comment.operation.as_deref(), Some("AddComment"));
        assert_eq!(comment.variables.unwrap()["body"], json!("Looks good"));
    }
}
