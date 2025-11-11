#[cfg(target_os = "macos")]
mod macos_sandbox_tests {
    use command_executor::{CommandExecutor, DefaultCommandExecutor, SandboxedCommandExecutor};
    use sandbox::SandboxPolicy;
    use std::path::Path;
    use tempfile::tempdir;

    fn executor_with_policy(policy: SandboxPolicy) -> SandboxedCommandExecutor {
        SandboxedCommandExecutor::new(Box::new(DefaultCommandExecutor), policy, None)
    }

    fn workspace_policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![root.to_path_buf()],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    }

    #[tokio::test]
    async fn read_only_policy_blocks_writes() {
        let temp = tempdir().expect("tempdir");
        let working_dir = temp.path().to_path_buf();

        let executor = executor_with_policy(SandboxPolicy::ReadOnly);
        let result = executor
            .execute("echo blocked > denied.txt", Some(&working_dir))
            .await
            .expect("command result");
        assert!(
            !result.success,
            "read-only sandbox should deny file writes (output: {})",
            result.output
        );
        assert!(
            !working_dir.join("denied.txt").exists(),
            "file should not be created"
        );
    }

    #[tokio::test]
    async fn workspace_write_allows_within_root() {
        let temp = tempdir().expect("tempdir");
        let working_dir = temp.path().to_path_buf();

        let policy = workspace_policy(&working_dir);
        let executor = executor_with_policy(policy);
        let result = executor
            .execute("echo allowed > ok.txt", Some(&working_dir))
            .await
            .expect("command result");
        assert!(
            result.success,
            "workspace write policy should allow writes (output: {})",
            result.output
        );
        assert!(
            working_dir.join("ok.txt").exists(),
            "file should be created"
        );
    }

    #[tokio::test]
    async fn workspace_write_blocks_outside_root() {
        let temp_parent = tempdir().expect("tempdir");
        let working_dir = temp_parent.path().join("project");
        std::fs::create_dir_all(&working_dir).expect("create project dir");

        let policy = workspace_policy(&working_dir);
        let executor = executor_with_policy(policy);
        let result = executor
            .execute("echo nope > ../outside.txt", Some(&working_dir))
            .await
            .expect("command result");
        assert!(
            !result.success,
            "workspace write policy should block writes outside root (output: {})",
            result.output
        );
        assert!(
            !temp_parent.path().join("outside.txt").exists(),
            "outside file should not be created"
        );
    }
}

#[cfg(not(target_os = "macos"))]
mod sandbox_tests_placeholder {
    #[test]
    fn sandbox_tests_skip_on_non_macos() {
        // Seatbelt enforcement is macOS-specific, so these tests are skipped elsewhere.
        assert!(true);
    }
}
