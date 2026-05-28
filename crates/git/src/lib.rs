mod binary;
mod branch;
mod repository;
mod types;
pub mod worktree;

pub use binary::GitBinary;
pub use branch::BranchNotMerged;
pub use repository::GitRepository;
pub use types::*;

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;

    fn git_config(dir: &Path, key: &str, value: &str) {
        std::process::Command::new("git")
            .args(["config", "--local", key, value])
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git config {key}: {e}"));
    }

    fn init_with_config(dir: &Path) -> gix::Repository {
        let mut repo = gix::init(dir).expect("failed to init repo");

        const CONFIGS: &[(&str, &str)] = &[
            // Set identity so commits work without global git config interference
            ("user.email", "test@test.com"),
            ("user.name", "Test"),
            // Ensure git commands are durable during I/O-contentious tests
            ("core.fsync", "all"),
            // Avoid signing
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            // Disable maintanance tasks
            ("maintenance.auto", "false"),
            ("gc.auto", "0"),
        ];

        for (key, value) in CONFIGS {
            git_config(dir, key, value);
        }
        repo.reload().expect("reload after config write");
        repo
    }

    /// Init a repo with identity + fsync config, then create an initial empty-tree commit.
    pub fn init_repo_with_commit(dir: &Path) -> gix::Repository {
        let repo = init_with_config(dir);
        let empty_tree = gix::ObjectId::empty_tree(repo.object_hash());
        repo.commit(
            "HEAD",
            "Initial commit",
            empty_tree,
            [] as [gix::ObjectId; 0],
        )
        .expect("failed to create initial commit");
        repo
    }

    /// Like `init_repo_with_commit` but without the commit.
    pub fn init_repo(dir: &Path) -> gix::Repository {
        init_with_config(dir)
    }
}
