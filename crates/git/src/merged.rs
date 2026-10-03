//! Whether a branch's work has landed in the base branch.

use crate::repository::GitRepository;
use anyhow::{Context, Result, bail};

impl GitRepository {
    /// The branch feature work merges into: the branch `origin/HEAD` points
    /// at when the remote declares one, otherwise the first of `origin/main`,
    /// `origin/master`, `main`, `master` that exists. Remote-tracking branches
    /// come first because that is where a merge lands before a local pull.
    pub fn default_base_branch(&self) -> Option<String> {
        let repo = self.repo.to_thread_local();
        if let Ok(head) = repo.find_reference("refs/remotes/origin/HEAD")
            && let gix::refs::TargetRef::Symbolic(name) = head.target()
        {
            return Some(name.shorten().to_string());
        }
        ["origin/main", "origin/master", "main", "master"]
            .into_iter()
            .find(|candidate| repo.find_reference(*candidate).is_ok())
            .map(str::to_string)
    }

    /// Whether every change on `branch` is contained in `base`.
    ///
    /// A regular or rebase merge leaves the branch tip as an ancestor of the
    /// base. A squash merge does not, so the branch's changes since the merge
    /// base are compared patch-wise instead (`git cherry` against a throwaway
    /// commit holding the branch tree on top of the merge base). A branch
    /// that does not exist is an error, not "merged".
    pub async fn is_branch_merged(&self, branch: &str, base: &str) -> Result<bool> {
        let workdir = self.workdir();
        let status = self
            .git
            .command(workdir)
            .args(["merge-base", "--is-ancestor", branch, base])
            .output()
            .await
            .context("Failed to execute git merge-base")?;
        match status.status.code() {
            Some(0) => return Ok(true),
            Some(1) => {}
            code => bail!(
                "git merge-base --is-ancestor {branch} {base} failed (exit {}): {}",
                code.unwrap_or(-1),
                String::from_utf8_lossy(&status.stderr).trim()
            ),
        }

        let merge_base = self.git.run(workdir, &["merge-base", base, branch]).await?;
        let tree = self
            .git
            .run(workdir, &["rev-parse", &format!("{branch}^{{tree}}")])
            .await?;
        if tree.is_empty() {
            bail!("{branch} has no tree");
        }
        let squashed = self
            .git
            .run(
                workdir,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &merge_base,
                    "-m",
                    "squash check",
                ],
            )
            .await?;
        let cherry = self.git.run(workdir, &["cherry", base, &squashed]).await?;
        Ok(!cherry.is_empty() && cherry.lines().all(|line| line.starts_with('-')))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;
    use std::path::Path;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit_file(dir: &Path, rel: &str, content: &str, message: &str) {
        std::fs::write(dir.join(rel), content).unwrap();
        git(dir, &["add", rel]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    /// A repo with a base branch holding one file and a `feature` branch
    /// with two commits on top. Returns the base branch name.
    fn repo_with_feature(dir: &Path) -> String {
        init_repo_with_commit(dir);
        let repo = GitRepository::open(dir).unwrap();
        let base = repo.current_branch().unwrap();
        commit_file(dir, "base.txt", "base\n", "base");
        git(dir, &["checkout", "-q", "-b", "feature"]);
        commit_file(dir, "one.txt", "one\n", "one");
        commit_file(dir, "two.txt", "two\n", "two");
        git(dir, &["checkout", "-q", &base]);
        base
    }

    #[tokio::test]
    async fn an_unmerged_branch_is_not_merged() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        let repo = GitRepository::open(dir.path()).unwrap();
        assert!(!repo.is_branch_merged("feature", &base).await.unwrap());
    }

    #[tokio::test]
    async fn a_fast_forwarded_branch_is_merged() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        git(dir.path(), &["merge", "-q", "--ff-only", "feature"]);
        let repo = GitRepository::open(dir.path()).unwrap();
        assert!(repo.is_branch_merged("feature", &base).await.unwrap());
    }

    #[tokio::test]
    async fn a_squash_merged_branch_is_merged_even_after_base_moves_on() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        git(dir.path(), &["merge", "-q", "--squash", "feature"]);
        git(dir.path(), &["commit", "-q", "-m", "feature (squashed)"]);
        commit_file(dir.path(), "later.txt", "later\n", "later work on base");
        let repo = GitRepository::open(dir.path()).unwrap();
        assert!(repo.is_branch_merged("feature", &base).await.unwrap());
    }

    #[tokio::test]
    async fn work_added_after_the_squash_merge_counts_as_unmerged() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        git(dir.path(), &["merge", "-q", "--squash", "feature"]);
        git(dir.path(), &["commit", "-q", "-m", "feature (squashed)"]);
        git(dir.path(), &["checkout", "-q", "feature"]);
        commit_file(dir.path(), "three.txt", "three\n", "three");
        git(dir.path(), &["checkout", "-q", &base]);
        let repo = GitRepository::open(dir.path()).unwrap();
        assert!(!repo.is_branch_merged("feature", &base).await.unwrap());
    }

    #[tokio::test]
    async fn a_missing_branch_is_an_error() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        let repo = GitRepository::open(dir.path()).unwrap();
        assert!(repo.is_branch_merged("nope", &base).await.is_err());
    }

    #[test]
    fn the_default_base_is_the_local_main_branch_without_a_remote() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        let repo = GitRepository::open(dir.path()).unwrap();
        if ["main", "master"].contains(&base.as_str()) {
            assert_eq!(repo.default_base_branch().as_deref(), Some(base.as_str()));
        } else {
            assert_eq!(repo.default_base_branch(), None);
        }
    }

    #[test]
    fn the_default_base_follows_origin_head() {
        let dir = TempDir::new().unwrap();
        let base = repo_with_feature(dir.path());
        let repo = GitRepository::open(dir.path()).unwrap();
        // A remote whose HEAD names a branch that is not main/master.
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/develop", "HEAD"],
        );
        git(
            dir.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/develop",
            ],
        );
        drop(base);
        let repo = GitRepository::open(repo.workdir()).unwrap();
        assert_eq!(
            repo.default_base_branch().as_deref(),
            Some("origin/develop")
        );
    }
}
