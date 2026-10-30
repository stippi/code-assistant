//! Rebasing a stack of branches, ported from T3 Code's
//! `cascadeRebaseStack`: in a scratch repository (the user's checkouts
//! stay untouched) each layer is replayed onto the rebased layer below,
//! bottom to top, and pushed with a lease on the head it had. A layer that
//! conflicts stops the cascade; the layers pushed before it stay.
//!
//! The credential travels in the environment (`GIT_CONFIG_*`), never on a
//! command line.

use crate::GitBinary;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Each git command may take this long.
const STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// A branch of the stack and the head it is expected at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackBranch {
    pub number: u64,
    pub branch: String,
    pub head_sha: String,
}

/// Where a cascade stopped, in T3 Code's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackRebaseError {
    Conflict {
        number: u64,
        completed: usize,
    },
    Git {
        step: &'static str,
        number: u64,
        completed: usize,
        detail: String,
    },
}

impl std::fmt::Display for StackRebaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackRebaseError::Conflict { number, completed } => write!(
                f,
                "PR #{number} conflicts with the layer below it. {completed} layers were \
                 rebased; resolve #{number} with `gh stack rebase` and push."
            ),
            StackRebaseError::Git {
                step,
                number,
                completed,
                ..
            } => write!(
                f,
                "Stack rebase stopped at PR #{number} while {step}. {completed} layers were \
                 rebased."
            ),
        }
    }
}

impl std::error::Error for StackRebaseError {}

/// A scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Cascade<'a> {
    git: &'a GitBinary,
    dir: &'a Path,
    /// `GIT_CONFIG_KEY_n` / `VALUE_n` pairs.
    config: Vec<(String, String)>,
}

impl Cascade<'_> {
    /// Run `git args`; its stdout, trimmed.
    async fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut command = self.git.command(self.dir);
        command.env("GIT_CONFIG_COUNT", self.config.len().to_string());
        for (ix, (key, value)) in self.config.iter().enumerate() {
            command.env(format!("GIT_CONFIG_KEY_{ix}"), key);
            command.env(format!("GIT_CONFIG_VALUE_{ix}"), value);
        }
        command.args(args);
        let output = tokio::time::timeout(STEP_TIMEOUT, command.output())
            .await
            .map_err(|_| anyhow::anyhow!("git {} timed out", args.join(" ")))??;
        if !output.status.success() {
            anyhow::bail!(
                "git {} failed: {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// Rebase `layers` (bottom to top) of the repository at `remote_url` onto
/// `base`: each layer onto the rebased layer below, pushed when it moved.
/// `auth_header` is an `http.extraheader` value for the remote.
pub async fn rebase_stack(
    git: &GitBinary,
    remote_url: &str,
    auth_header: &str,
    base: &str,
    layers: &[StackBranch],
) -> Result<(), StackRebaseError> {
    let Some(first) = layers.first() else {
        return Ok(());
    };
    let failed = |step: &'static str, number: u64, completed: usize| {
        move |error: anyhow::Error| StackRebaseError::Git {
            step,
            number,
            completed,
            detail: format!("{error:#}"),
        }
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "code-assistant-stack-rebase-{}-{nanos}",
        std::process::id()
    )));
    std::fs::create_dir_all(&scratch.0)
        .map_err(|e| failed("preparing", first.number, 0)(e.into()))?;
    let mut cascade = Cascade {
        git,
        dir: &scratch.0,
        config: vec![(
            format!("http.{remote_url}.extraheader"),
            auth_header.to_owned(),
        )],
    };
    cascade
        .run(&["init", "--quiet"])
        .await
        .map_err(failed("preparing", first.number, 0))?;
    // Rebased commits need a committer; the user's own when configured.
    if cascade.run(&["config", "user.email"]).await.is_err() {
        cascade.config.extend([
            ("user.name".to_owned(), "code-assistant".to_owned()),
            (
                "user.email".to_owned(),
                "noreply@code-assistant.invalid".to_owned(),
            ),
        ]);
    }
    cascade
        .run(&["remote", "add", "origin", remote_url])
        .await
        .map_err(failed("preparing", first.number, 0))?;
    let mut refspecs = vec![format!("+refs/heads/{base}:refs/remotes/origin/{base}")];
    refspecs.extend(
        layers
            .iter()
            .map(|l| format!("+refs/heads/{0}:refs/remotes/origin/{0}", l.branch)),
    );
    let mut fetch = vec!["fetch", "--quiet", "--no-tags", "origin"];
    fetch.extend(refspecs.iter().map(String::as_str));
    cascade
        .run(&fetch)
        .await
        .map_err(failed("fetching", first.number, 0))?;

    let base_ref = format!("refs/remotes/origin/{base}");
    let (mut parent_old, mut parent_new) = (base_ref.clone(), base_ref.clone());
    for (completed, layer) in layers.iter().enumerate() {
        let number = layer.number;
        let upstream = match completed {
            0 => cascade
                .run(&["merge-base", &base_ref, &layer.head_sha])
                .await
                .map_err(failed("reading the fork point", number, completed))?,
            _ => parent_old.clone(),
        };
        cascade
            .run(&["checkout", "--quiet", "--detach", &layer.head_sha])
            .await
            .map_err(failed("checking out", number, completed))?;
        if cascade
            .run(&["rebase", "--quiet", "--onto", &parent_new, &upstream])
            .await
            .is_err()
        {
            let _ = cascade.run(&["rebase", "--abort"]).await;
            return Err(StackRebaseError::Conflict { number, completed });
        }
        let rebased = cascade
            .run(&["rev-parse", "HEAD"])
            .await
            .map_err(failed("rebasing", number, completed))?;
        if rebased != layer.head_sha {
            let lease = format!(
                "--force-with-lease=refs/heads/{}:{}",
                layer.branch, layer.head_sha
            );
            let target = format!("{rebased}:refs/heads/{}", layer.branch);
            cascade
                .run(&["push", "--quiet", &lease, "origin", &target])
                .await
                .map_err(failed("pushing", number, completed))?;
        }
        parent_old = layer.head_sha.clone();
        parent_new = rebased;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", file]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// A bare "GitHub" with `main`, `one` on it and `two` on `one`; then
    /// `main` moves on. The work happens in `work`, pushed to the remote.
    fn stacked() -> (tempfile::TempDir, PathBuf, Vec<StackBranch>) {
        let dir = tempfile::TempDir::new().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        run(
            dir.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        std::fs::create_dir(&work).unwrap();
        run(&work, &["init", "-q", "-b", "main"]);
        commit(&work, "base", "base");
        run(&work, &["checkout", "-q", "-b", "one"]);
        let one = commit(&work, "one", "one");
        run(&work, &["checkout", "-q", "-b", "two"]);
        let two = commit(&work, "two", "two");
        run(&work, &["checkout", "-q", "main"]);
        commit(&work, "later", "later");
        run(
            &work,
            &["push", "-q", remote.to_str().unwrap(), "main", "one", "two"],
        );
        let layers = vec![
            StackBranch {
                number: 1,
                branch: "one".into(),
                head_sha: one,
            },
            StackBranch {
                number: 2,
                branch: "two".into(),
                head_sha: two,
            },
        ];
        (dir, remote, layers)
    }

    #[tokio::test]
    async fn every_layer_lands_on_the_rebased_layer_below() {
        let (_dir, remote, layers) = stacked();
        let git = GitBinary::new().unwrap();
        let url = remote.to_str().unwrap();
        rebase_stack(&git, url, "AUTHORIZATION: basic x", "main", &layers)
            .await
            .unwrap();
        let parent = |branch: &str| run(&remote, &["rev-parse", &format!("{branch}~1")]);
        let tip = |branch: &str| run(&remote, &["rev-parse", branch]);
        assert_eq!(parent("one"), tip("main"));
        assert_eq!(parent("two"), tip("one"));
        assert_ne!(tip("one"), layers[0].head_sha);
    }

    #[tokio::test]
    async fn a_branch_pushed_to_meanwhile_is_not_overwritten() {
        let (dir, remote, layers) = stacked();
        // Someone pushed to `one` since the stack was read.
        let work = dir.path().join("work");
        run(&work, &["checkout", "-q", "one"]);
        commit(&work, "extra", "extra");
        run(&work, &["push", "-q", remote.to_str().unwrap(), "one"]);
        let git = GitBinary::new().unwrap();
        let stopped = rebase_stack(&git, remote.to_str().unwrap(), "x: y", "main", &layers)
            .await
            .unwrap_err();
        assert!(
            matches!(
                stopped,
                StackRebaseError::Git {
                    step: "pushing",
                    number: 1,
                    completed: 0,
                    ..
                }
            ),
            "{stopped:?}"
        );
    }
}
