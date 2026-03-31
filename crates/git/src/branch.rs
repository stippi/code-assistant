use crate::repository::GitRepository;
use crate::types::Branch;
use anyhow::Result;

impl GitRepository {
    /// List local branches using `git2`.
    ///
    /// This is fast because it runs in-process via libgit2 with no
    /// subprocess overhead.
    pub fn list_branches(&self) -> Result<Vec<Branch>> {
        let mut branches = Vec::new();

        let head_name: Option<String> = self
            .repo
            .head()
            .ok()
            .and_then(|h: git2::Reference<'_>| h.shorthand().map(String::from));

        for entry in self.repo.branches(Some(git2::BranchType::Local))? {
            let (branch, _branch_type): (git2::Branch<'_>, git2::BranchType) = entry?;
            let name = branch.name()?.unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }

            let upstream = branch
                .upstream()
                .ok()
                .and_then(|u: git2::Branch<'_>| u.name().ok().flatten().map(String::from));

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
    /// Uses the git CLI because libgit2 branch creation doesn't
    /// trigger hooks and has limited reflog support.
    pub async fn create_branch(&self, name: &str, start_point: &str) -> Result<()> {
        self.git
            .run(self.workdir(), &["branch", name, start_point])
            .await?;
        Ok(())
    }

    /// Switch the working directory to the given branch.
    pub async fn checkout_branch(&self, name: &str) -> Result<()> {
        self.git.run(self.workdir(), &["checkout", name]).await?;
        Ok(())
    }

    /// Delete a local branch. Set `force` to allow deleting unmerged branches.
    pub async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.git
            .run(self.workdir(), &["branch", flag, name])
            .await?;
        Ok(())
    }

    /// Get the current branch name, or `None` if in detached HEAD state.
    pub fn current_branch(&self) -> Option<String> {
        self.repo.head().ok().and_then(|head: git2::Reference<'_>| {
            if head.is_branch() {
                head.shorthand().map(String::from)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo_with_commit(dir: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            let tree_id = {
                let mut index = repo.index().unwrap();
                // Create a dummy file so we have something to commit
                let file_path = dir.join("README.md");
                std::fs::write(&file_path, "# Test\n").unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                index.write_tree().unwrap()
            };
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                .unwrap();
        }
        repo
    }

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

    #[tokio::test]
    async fn create_and_list_branch() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        repo.create_branch("feature-x", "HEAD").await.unwrap();

        let branches = repo.list_branches().unwrap();
        assert_eq!(branches.len(), 2);

        let feature = branches.iter().find(|b| b.name == "feature-x").unwrap();
        assert!(!feature.is_head);
    }
}
