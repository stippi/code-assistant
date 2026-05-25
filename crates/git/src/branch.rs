use crate::repository::GitRepository;
use crate::types::Branch;
use anyhow::{Context, Result};
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

impl GitRepository {
    /// List local branches using `gix`.
    ///
    /// This is fast because it runs in-process via gix with no
    /// subprocess overhead.
    pub fn list_branches(&self) -> Result<Vec<Branch>> {
        let repo = self.repo.to_thread_local();
        let mut branches = Vec::new();

        let head_name: Option<String> = repo
            .head_name()
            .ok()
            .flatten()
            .map(|n| n.shorten().to_string());

        for reference in repo.references()?.local_branches()? {
            let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = reference.name().shorten().to_string();
            if name.is_empty() {
                continue;
            }

            let upstream = repo
                .branch_remote_ref_name(reference.name(), gix::remote::Direction::Fetch)
                .and_then(Result::ok)
                .map(|r| r.shorten().to_string());

            branches.push(Branch {
                is_head: head_name.as_deref() == Some(&name),
                name,
                upstream,
            });
        }

        // Sort: HEAD branch first, then alphabetical
        branches.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));

        Ok(branches)
    }

    /// Create a new local branch pointing at `start_point`.
    ///
    /// Uses gix in-process ref creation.
    pub fn create_branch(&self, name: &str, start_point: &str) -> Result<()> {
        let repo = self.repo.to_thread_local();
        let target_id = repo
            .rev_parse_single(start_point)
            .with_context(|| format!("failed to resolve '{start_point}'"))?
            .detach();
        let full_name = format!("refs/heads/{name}");
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("branch: Created from {start_point}").into(),
                },
                expected: PreviousValue::MustNotExist,
                new: Target::Object(target_id),
            },
            name: full_name.try_into().context("invalid branch name")?,
            deref: false,
        })
        .with_context(|| format!("failed to create branch '{name}'"))?;
        Ok(())
    }

    /// Switch the working directory to the given branch.
    ///
    /// Uses the git CLI because checkout updates the worktree and fires the
    /// post-checkout hook.
    pub async fn checkout_branch(&self, name: &str) -> Result<()> {
        self.git.run(self.workdir(), &["checkout", name]).await?;
        Ok(())
    }

    /// Delete a local branch. Set `force` to allow deleting unmerged branches.
    ///
    /// Uses gix in-process ref deletion.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let repo = self.repo.to_thread_local();
        let full_name = format!("refs/heads/{name}");

        let expected = if force {
            PreviousValue::Any
        } else {
            PreviousValue::MustExist
        };

        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected,
                log: RefLog::AndReference,
            },
            name: full_name.try_into().context("invalid branch name")?,
            deref: false,
        })
        .with_context(|| format!("failed to delete branch '{name}'"))?;
        Ok(())
    }

    /// Get the current branch name, or `None` if in detached HEAD state.
    pub fn current_branch(&self) -> Option<String> {
        self.repo
            .to_thread_local()
            .head_name()
            .ok()
            .flatten()
            .map(|name| name.shorten().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;
    use tempfile::TempDir;

    #[test]
    fn list_branches_on_new_repo() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        let branches = repo.list_branches().unwrap();

        assert_eq!(branches.len(), 1);
        // git init creates a default branch (usually main or master)
        assert!(branches[0].is_head);
    }

    #[test]
    fn current_branch_on_new_repo() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        let branch = repo.current_branch();
        assert!(branch.is_some());
    }

    #[test]
    fn create_and_list_branch() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        repo.create_branch("feature-x", "HEAD").unwrap();

        let branches = repo.list_branches().unwrap();
        assert_eq!(branches.len(), 2);

        let feature = branches.iter().find(|b| b.name == "feature-x").unwrap();
        assert!(!feature.is_head);
    }

    #[test]
    fn delete_branch() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        repo.create_branch("to-delete", "HEAD").unwrap();
        assert_eq!(repo.list_branches().unwrap().len(), 2);

        repo.delete_branch("to-delete", false).unwrap();
        let branches = repo.list_branches().unwrap();
        assert_eq!(branches.len(), 1);
        assert!(branches.iter().all(|b| b.name != "to-delete"));
    }
}
