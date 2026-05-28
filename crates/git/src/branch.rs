use crate::repository::GitRepository;
use crate::types::Branch;
use anyhow::{Context, Result};
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

/// Returned by `delete_branch` when a non-force delete is refused because the
/// branch still has unmerged commits. The UI can downcast to detect this and
/// offer a force retry.
#[derive(Debug)]
pub struct BranchNotMerged(pub String);

impl std::fmt::Display for BranchNotMerged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "branch '{}' is not fully merged", self.0)
    }
}
impl std::error::Error for BranchNotMerged {}

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
    /// Without `force`, this will refuse to delete:
    /// - The currently checked-out branch
    /// - Branches with unmerged commits (returns a `BranchNotMerged` error that
    ///   the UI can downcast to detect and offer a force retry)
    ///
    /// Uses gix in-process ref deletion.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let repo = self.repo.to_thread_local();
        let full_name = format!("refs/heads/{name}");

        if !force {
            // Never delete the branch HEAD currently points at (matches git).
            if self.current_branch().as_deref() == Some(name) {
                anyhow::bail!("cannot delete branch '{name}': it is currently checked out");
            }
            if !is_branch_merged(&repo, &full_name)? {
                return Err(anyhow::Error::new(BranchNotMerged(name.to_string())));
            }
        }

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

/// True if the branch tip is reachable from its upstream (if set) or HEAD.
fn is_branch_merged(repo: &gix::Repository, full_name: &str) -> Result<bool> {
    let branch_ref = repo
        .find_reference(full_name)
        .with_context(|| format!("branch '{full_name}' not found"))?;
    let branch_tip = branch_ref.clone().into_fully_peeled_id()?.detach();

    // Prefer the local remote-tracking ref (refs/remotes/...); fall back to HEAD.
    let base_tip = match repo
        .branch_remote_tracking_ref_name(branch_ref.name(), gix::remote::Direction::Fetch)
    {
        Some(Ok(tracking)) => repo
            .find_reference(tracking.as_ref())?
            .into_fully_peeled_id()?
            .detach(),
        _ => repo.head_id()?.detach(),
    };

    // Merged iff branch_tip is an ancestor of base_tip,
    // i.e. their merge base IS branch_tip.
    let merge_base = repo.merge_base(branch_tip, base_tip)?.detach();
    Ok(merge_base == branch_tip)
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
    fn delete_branch_merged() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        // Branch created from HEAD is already merged (tip == HEAD)
        repo.create_branch("to-delete", "HEAD").unwrap();
        assert_eq!(repo.list_branches().unwrap().len(), 2);

        repo.delete_branch("to-delete", false).unwrap();
        let branches = repo.list_branches().unwrap();
        assert_eq!(branches.len(), 1);
        assert!(branches.iter().all(|b| b.name != "to-delete"));
    }

    #[test]
    fn delete_branch_unmerged_without_force_fails() {
        let dir = TempDir::new().unwrap();
        let gix_repo = init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        repo.create_branch("feature", "HEAD").unwrap();

        // Add a commit on `feature` that isn't on the default branch.
        // We do this by updating the feature ref to a new commit.
        let head_id = gix_repo.head_id().unwrap().detach();
        let empty_tree = gix::ObjectId::empty_tree(gix_repo.object_hash());
        let new_commit = gix_repo
            .commit(
                "refs/heads/feature",
                "feature commit",
                empty_tree,
                [head_id],
            )
            .unwrap();
        // Sanity: feature now points to a different commit than HEAD
        assert_ne!(new_commit.detach(), head_id);

        let err = repo.delete_branch("feature", false).unwrap_err();
        // Should be a BranchNotMerged error
        assert!(err.downcast_ref::<BranchNotMerged>().is_some());
        // Branch should still exist
        assert_eq!(repo.list_branches().unwrap().len(), 2);
    }

    #[test]
    fn delete_branch_unmerged_with_force_succeeds() {
        let dir = TempDir::new().unwrap();
        let gix_repo = init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        repo.create_branch("feature", "HEAD").unwrap();

        // Add a commit on `feature` that isn't on the default branch.
        let head_id = gix_repo.head_id().unwrap().detach();
        let empty_tree = gix::ObjectId::empty_tree(gix_repo.object_hash());
        gix_repo
            .commit(
                "refs/heads/feature",
                "feature commit",
                empty_tree,
                [head_id],
            )
            .unwrap();

        // Force delete should succeed
        repo.delete_branch("feature", true).unwrap();
        assert_eq!(repo.list_branches().unwrap().len(), 1);
    }

    #[test]
    fn delete_current_branch_without_force_fails() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        let current = repo.current_branch().unwrap();

        let err = repo.delete_branch(&current, false).unwrap_err();
        assert!(
            err.to_string().contains("currently checked out"),
            "unexpected error: {err}"
        );
    }
}
